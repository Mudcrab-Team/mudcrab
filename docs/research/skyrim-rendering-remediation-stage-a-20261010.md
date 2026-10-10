# Skyrim rendering remediation: first implementation slice

This receipt covers R0 checkpoint tooling, part of R1 capture instrumentation,
R3 source preservation and a bounded static extension of R2. It does not accept
the current renderer's appearance or close Skyrim's output numeric domain.
Gamma, exposure, saturation, ambient, fog and shadow response defaults were not
changed by this slice. The brighter checkpoint remains available through its
hash-checking launcher.

## Implemented behavior

[Checkpoint tooling](../../scripts/skyrim-render-checkpoint.py) preserves the
dirty source tree, tracked patch, local vendor dependencies, binary, asset
manifest, requested profile and shot fixture in a new directory outside the
checkout. Verification checks the complete saved inventory, contents and
artifact identities; optional external checks detect changed inputs. It rejects
extra automatic build inputs, including an unrecorded `.cargo/config.toml`.
Source symlinks retain their targets; external symlink referents are not frozen.
The current candidate inventory contains no source symlinks. Asset-manifest
identity is recorded separately from full pack-content validation.

[Capture receipts](../../crates/engine/src/capture_receipt.rs) cover `--shots`
and headless `--acceptance-screenshot`. Each PNG has an immutable `.capture.json`
sidecar with distinct requested, extracted and readback-completed
frame identities, camera and output-view joins, image hashes, adapter/format
state, resolved environment fields and selected image-space float readbacks.
Capture diagnostics are warmed up in advance and attached to that request's
render frame instead of borrowing the periodically overwritten report. GPU
readback supplies the sample coordinates, UVs and extents. Periodic diagnostics
track each render view; capture diagnostics track request IDs. Eight outstanding
readbacks are allowed. A second opted-in camera no longer starves behind the
first; an exhausted bound leaves evidence missing.
Existing PNGs, sidecars, summaries and temporary files are preserved.
Interactive window acceptance screenshots remain outside this receipt route.

An attached source receipt is graded as an attachment and executable hash match.
Capture does not rerun the checkpoint verifier. The request/view/diagnostic join
has its own verdict. The actual Bevy screenshot GPU copy frame and private
transfer source are unobserved, so `image_frame_join` and overall
`capture_coherence` remain `pending`. Inconsistent observed frame, camera,
request, output-view generation, dimensions or callback chronology produce
`fail`. These observations do not prove native draw coverage, all bindings,
arbitrary stage readbacks, selected retail execution or appearance.
Profiling dimensions now come from the actual active camera target. Build profile
stays unknown in runtime metadata; the external build receipt records `quick`.

The float diagnostic samples the scene input and replays shared tone/bridge
arithmetic in RGBA32Float. It is not a dump of the production pass output.
Bevy tone mapping, FXAA and upscaling/output follow it. Camera FXAA settings and
pipeline readiness are recorded separately from actual pass execution. Direct
float-to-PNG equality needs a validated or explicitly controlled remaining
output chain.

[Native diffuse publication](../../crates/converter/src/native_diffuse.rs)
requalifies the winning staged DDS on every conversion, including cached NIF
outputs. Qualified ordinary 2D legacy DXT1/3/5 and typed DX10 BC1/2/3/7 views
preserve the declared mip blocks and transfer in independent KTX2 aliases.
The semantic PBR view remains separate. Unsupported formats, resource layouts,
unaccounted payload bytes and absent sources have explicit qualification status
and no usable native declaration. Runtime rejects explicit unqualified views.
Older declarations without qualification retain compatibility, which does not
establish their source qualification.

Dependency collection includes declared native views. Publication updates GLB
and manifest identities, removes stale native aliases and preserves mesh BIN
data, other texture data and pruning provenance. Full converter check validates
declared qualified KTX2 resource shape, exact compressed format and transfer in
addition to manifest file identities. It requires authored diffuse-slot/view
coverage for current meshes; retained older meshes keep their explicit boundary.
Qualified native/PBR files must be contained and distinct; Unix checks also
reject two paths to the same inode.
Full source/binding/GLB closure and retail use remain separate checks.

Lighting Effect 1/2 are retained as optional raw binary32 bit fields, including
signed zero and NaN payloads. Their preservation adds no subsurface or lighting
response. Producer 27 rebuilds older GLBs, while verified canonical PBR textures
from producers 24–26 and scripts/archives from 12–26 remain reusable. The runtime
accepts matching LOD producer pairs 26/26 or 27/27 under the existing world-9,
scale, identity and chunk-count guards. Mixed and unknown producers fail.
Metadata refresh retains its specifically verified terrain migration and records
the older mesh producer rather than claiming new surface data. Exact historical
prune replay may remove only the two new opaque fields for already-admitted
producers through 26, and only when both the recorded original hash/size and
retained prune bytes match. Current and unknown producers cannot use that
projection.

## Native output extension

The [output-transfer report](skyrim-render-map/native-output-transfer.md) follows
requested target 0 through cached image-space output references, backbuffer
RTV/SRV imports, OM binding and Present. It retains conditional TAA/upsample
paths and the unresolved later composition client. The UNORM endpoint supplies
no gamma, exposure or numeric-domain default.

The extension has five read-only queries, 23 independently decoded functions
and 3,633 original instruction starts. Its 33 curated receipts cover 1,569
instruction starts, 24 rel32 transfers and five SDK call offsets. These overlap
earlier evidence and are not added to the central 45-query/85,200-start union.
The checker preserves historical material/mesh source pins and records current
source differences. It validates those historical files against the frozen
checkpoint selected with `--project-source-evidence`.

Read-only Fiji discovery matched the installed executable and shader archive
to the pinned local files by SHA-256 and size. Steam selects `GE-Proton`; a
read-only followup resolved that manifest name to the installed GE-Proton11-5
entrypoint through `STEAM_EXTRA_COMPAT_TOOLS_PATHS`. Gamescope advertises a
headless backend, but game startup and capture through it have not been proved.
The private `retail-runner-readiness.json` preserves the first probe and launch
templates; `retail-runner-ge-proton-resolution.json` records the later tool
resolution. No retail game was launched during either probe.

## Validation and candidate identity

The implemented slice passed the following checks on Apple M1 Pro/Metal. These
are separate gates; they do not accept the current appearance.

| Check | Result | Boundary |
| --- | --- | --- |
| Checkpoint integrity | 11 tests passed, plus candidate creation and external verification | Saved source/artifact identity; pack contents and external symlink referents remain separate. |
| Converter | 607 passed, 0 failed, 14 ignored across 39 targets | Includes native publication, historical replay, resource rejection and reviewed snapshots. Installed-asset/performance probes remain outside this run. |
| Engine library | 472 passed, 0 failed | Includes capture collisions/joins and per-view diagnostic capacity tests. |
| CLI | 5 passed | Supported argument parsing; no retail session. |
| Fog GPU | 1 test, nine regions passed | Scoped production shader response, not whole-game fog parity. |
| Native lighting GPU | 28 cases passed | Injected-input material response, not effective retail light selection. |
| Image-space arithmetic GPU | 12 cases passed | Selected equations and format bounds. |
| Packed image-space scene GPU | Eight cases and 40 float checks passed | Required `Rg11b10Ufloat`, with no fallback accepted. |
| GPU coordinate control | Eight coordinate checks passed; all 16 one-pixel misattributions rejected | Static checkerboard in an offset, odd-size viewport; no general screenshot-frame proof. |

The three original Riverwood shots completed settled with passing
request/view/diagnostic joins. Their PNG hashes match the sidecars, all 24
GPU-reported coordinates are valid, and each sidecar has no observed coherence
error. The image-frame and overall joins remain pending. Headless renderer-fixture
capture also completed at 1600×900 with the same explicit image-copy boundary.

The first candidate's proposed direct float-to-PNG check found nine differences
among 24 samples, up to 84 RGB codes. That check had omitted the intervening
FXAA stage. Fresh capture with GPU-reported coordinates retains nine conditional
differences and the same maximum. Enabled HIGH/HIGH FXAA is observed in the
camera state; no attribution of those differences to its actual execution is
established. The revised audit leaves output equivalence pending. The original
failed check and captures are preserved.

Private artifacts are under
`/Users/taylor/.local/share/mudcrab-research/lighting-remediation-20261010/`.
The first candidate checkpoint is `checkpoints/candidate-r01-r3-01`, with 424
source entries and four preserved artifacts. Its receipt SHA-256 is
`cb0f0f05d3db735a7d3412fda440e6da88113abf09a32894c95977a29fbc0a72`.
This identifies the first source snapshot and captured executable. The final
instrumented binary is SHA-256
`fda96381c6e89966164a9f7d85136b9791dc8e2b4e6e32e9f2b404332f36ab30`.
The three-shot replay is `captures/candidate-02`, attached to
`checkpoints/candidate-r01-r3-02/receipt.json`, SHA-256
`620ed855f1bbdfb95303c3f07afcdca09d6a6a9d028eed35e18623555e31a01b`.
That snapshot has 429 source entries and four artifacts. Its source-tree hash is
`c93266021417d7561aae10ca6f48e40b289813436d7eb5cf76953b105b1400c3`.
Later documentation edits are not retroactively added to capture receipts.

Build and test logs, probe JSON, checkpoints and capture sidecars remain under
the private root. Failed intermediate test runs are retained alongside the
passing reruns. The brighter binary and original lighting-pack manifest still
match their launcher pins.

## Imported reference comparisons

The [reference audit](skyrim-reference-import-20261010.md) verifies the three new
archives, 91 files, 78 images and 84 imported poses. The Inn image is an exact
2012 UESP original; the other sampled imported images have unresolved edition
or source-byte connections. No weather/time/settings receipts exist.

Two repository fixtures retain three numeric camera poses. The wide fixture
explicitly changes output aspect to 1400×788; the other two views retain 4:3.
Fresh candidate and brighter-checkpoint captures completed settled for all three
views. They are stored in `captures/reference-candidate-{4x3,wide}-01` and
`captures/reference-brighter-{4x3,wide}-01`. Candidate receipts again pass the
request/view/diagnostic join and leave the image-frame join pending. The old
brighter executable supplies PNGs without these new sidecars.

These comparisons support landmark fitting and qualitative appearance review.
They establish neither exact camera alignment nor a matched SE/AE lighting
result, and supply no new renderer coefficients.
The private `reference-comparison.html` displays the supplied reference and both
captured builds for each of the three views, with source credits and limits.

## Remaining gates

R1 still needs observed screenshot GPU copy/transfer identity, actual production
stage outputs, drawn-primitive/path coverage, arbitrary float stage/region
readbacks, exact bindings/constants, complete effective profiles and independent
diagnostic controls. R2 still needs a runnable retail capture route, selected
shader/resource/constant state and raw output pixels through the final transfer.
R4–R10 corrections and appearance acceptance depend on those owning contracts.
No matched retail frame exists at this checkpoint.

The full lighting pack has not been regenerated as producer 27. Native aliases
are rebuilt per conversion and Full check reads their referenced resources;
full-pack timing, I/O cost and release/target-hardware acceptance remain open.
The `quick` build and windowless captures establish functional behavior only.
