# Hybrid object LOD

## Direction and status

[Bill's #203 proposal](https://github.com/Mudcrab-Team/mudcrab/issues/203) and
[Taylor's agreement](https://github.com/Mudcrab-Team/mudcrab/issues/203#issuecomment-6086560142)
support a scoped cluster experiment within
[#103 native LOD](https://github.com/Mudcrab-Team/mudcrab/issues/103).
The accepted direction is meshoptimizer-based cluster selection with indirect
draws, sharing native residency, eligibility, validation and safe handoff.
Compare it with automatically generated conventional LOD at matched visual
quality. Terrain, small meshes, trees and unsupported materials keep their
appropriate paths. Reflections are independent work.

Shared eligibility and whole-reference handoff contracts now exist in
[`shared::lod::object`](../../../crates/shared/src/lod/object.rs), with explicit
verification and stale-generation checks. They do not yet create engine
entities, select object sources or control renderer visibility.

The payload and runtime choices below are proposed contracts for review. This document does not mark a cluster compiler, renderer, paging
system or HLOD implementation complete. Current terrain payloads and their
historical acceptance evidence remain described in
[LOD architecture](lod-architecture.md) and the
[compiler specification](../converters/lod-compiler.md). This amendment changes
no runtime default and assigns no numeric schema or format versions.

An [offline comparison adapter](../../../scripts/lod-experiment/README.md) now
builds conventional index tiers and a grouped cluster DAG from the same eligible
GLB primitives using pinned meshoptimizer v1.3. Synthetic tests check exact
source cuts, uniform group replacement, bounds/index validation, explicit
omissions and repeatability. Its JSON artifacts are experimental, not the
proposed runtime sidecar ABI. It does not classify world reference eligibility,
certify conservative error or visual quality, or implement a pipeline stage,
renderer, paging or HLOD. Those acceptance gates remain open.

Bill's converter spike used meshoptimizer v1.3 on 75 converted vanilla meshes,
1,487 primitives and 527k triangles, single-thread release, median of three
runs. Its reported time and size estimates justify an experiment; projected
whole-library and parallel costs are not full-converter measurements.
Runtime frame time remains unmeasured in that evidence.

## Ownership and routes

| Responsibility | Contract |
|---|---|
| World residency | Existing world/cell/chunk generations, spatial admission, shared budgets, unload and origin rebasing own availability. Geometry detail cannot attach a cell. |
| Eligibility and state | Resolved records and the runtime state owner decide which reference is enabled and allowed. Derived data cannot persist a competing state. |
| Geometry detail | Choose verified source, authored/generated conventional LOD, or a valid cluster cut for eligible resident objects. |
| Draw submission | Select compatible batches and a capability-supported command path without changing logical object identity or coverage. |

Start with verified, unconditionally enabled exterior `STAT` references and
opaque static primitives. Require authoritative base type, reference flags,
enable-parent metadata, model behavior, transforms and conservative bounds.
Omit unknown, initially disabled, conditional, movable, animated and unresolved
large-reference cases from static derivation with reasons until their state
and handoff route is validated. Initially-disabled filtering alone does not
prove complete enable-parent semantics.

Small primitives retain ordinary meshes below a measured threshold. Keep
alpha-tested surfaces out of the first cluster experiment; silhouette and mip
acceptance must precede their admission. Blended, skinned and animated meshes
keep supported ordinary/proxy routes. Trees use dedicated billboards or
impostors; terrain retains its GLB/chunk path. Missing source assets remain
explicit omissions. Missing authored variants may use validated automatic
generation; invalid or unavailable derived data retains a usable verified
source/conventional route.

## Shared object lifecycle

Keep resource identity separate from reference/placement identity. Each
render instance maps to its source object, primitive, material and current
world generation. Batching cannot erase that mapping. Derived representations
contain visual data only; scripts, collision, inventory, quests and AI stay
with the runtime owner.

The proposed mutation contract in
[#185](https://github.com/Mudcrab-Team/mudcrab/issues/185) requires an escape
path for individual enable/disable, movement, selection and material/model
changes. Hide affected derived instances and let the normal object path own
the changed reference; invalidate affected derived data. Never reveal stale
geometry while waiting for a replacement. Save overrides use durable source
identity and do not serialize renderer caches. This behavior needs fixtures
before derived batching or HLOD can be accepted.

Readiness means the required geometry, materials and textures are validated,
prepared and drawable for the relevant view. A resident cell or loaded root
alone is insufficient. Mixed-ready near objects transition individually, or
as an explicitly complete ownership unit; one ready model cannot hide an
entire object's missing coverage or unrelated references in its source cell.
Keep an eligible far representation until its corresponding replacement is
drawable, without persistent duplicate coverage.

Conventional and cluster routes share generation guards, invalidation and
failure diagnostics. Delayed/failed assets, unload, teleport, world changes
and origin rebasing cannot activate stale completions. Source moves and
material/eligibility changes invalidate every affected tier and placement.
Transient retries remain bounded under the owning streaming policy; failed
optional geometry never rewrites source metadata or gameplay state.

## Converter experiment and proposed sidecar

Resolve one immutable set of source primitives, winning materials and records.
Produce generated conventional LOD and cluster candidates from those same
inputs. Preserve validated authored variants as a third option where suitable.
Bill's up-to-128-triangle clusters and approximately halved groups are starting
parameters; investigate grouping, simplification limits and border behavior
rather than declaring those parameters a permanent format requirement.

Keep source model GLBs authoritative. Store clusters in an optional sidecar
with an explicit format version and algorithm/settings identity. Its manifest
must include source GLB and consumed material/texture hashes, primitive and
attribute layout identity, compiler/meshoptimizer revision and payload hash.
World placement and reference state remain in their owning world contracts.

The proposed serialized payload carries:

- Stable source primitive/material identity and index/attribute mappings.
  Preserve every attribute required by the supported material path, including
  UVs, normals, tangents and colors. Do not merge incompatible material,
  alpha, emissive or overlay families.
- Conservative geometry and error bounds with declared units and an error
  propagation convention. Parent/group error must conservatively account for
  represented source detail. Bounds must remain conservative under placement
  transforms. Bounds validity does not establish silhouette/material quality.
- Acyclic group replacement links and valid complete cuts. Grouped
  simplification may recluster geometry, so independent per-cluster tree
  parents cannot be assumed. Replacing a group requires all its replacement
  coverage; drawing arbitrary parent/child combinations can crack or overlap.
- Valid vertex/index ranges, preserved locked borders, and a complete coarse
  fallback for each admitted primitive/material unit. Validate degenerates,
  non-manifold input handling, finite error values and attribute seams.

Review byte order, integer widths, alignment, lengths, allocation limits,
version rejection and checksum boundaries before implementing the loader.
Use independent golden fixtures and malformed-payload cases. A meshoptimizer
demo structure is not a stable storage ABI. Filenames, numeric versions and
any database migration belong to the implementation review.

Integrate sidecars with existing input locks, hash verification, staged
publication and rollback. Do not relabel retained GLBs as a new producer.
Exact consumed-input, format, compiler, algorithm, settings and payload proof
controls reuse. Changed inputs rebuild affected derived data; removed inputs
or disabled generation remove owned obsolete sidecars. Missing, incompatible
or corrupt sidecars preserve verified source rendering and report the rejected
route. A requested experiment reports incomplete/error status explicitly;
fallback rendering does not certify successful generation.

Gate the converter experiment on reproducible clean/incremental results,
source-byte preservation, attribute/material/border checks and payload audit.
Record time, cache reuse, bytes and peak memory, plus measured geometric,
silhouette and material error across vanilla and representative mod assets.
No proprietary source assets enter the repository.

## Resident runtime experiment

Start with resident hierarchies and conservative screen-space error selection.
Scale geometric error with the object's transform and the view's projection
and viewport; test mirrored/nonuniform transforms and near-plane intersections.
Approximately one pixel is an experimental setting. Choose a complete legal
cut whose replacement groups are ready, with no missing coverage or simultaneous
parent/child duplicates. Test cuts and transitions in motion, not only stills.

Declare main, reflection and shadow coverage separately. Use per-view cuts or
a conservative combined policy, with explicit viewport/error and layer rules.
A main-camera cut or occlusion result cannot establish another view's coverage.
Keep shadow range independent of camera far; measure the cost of every view.

Probe adapter capabilities at startup. Native multi-draw with GPU draw count
may be used where supported; Metal needs a bounded path without that count
variant, such as fixed-capacity indirect slots with zeroed unused commands.
Measure the CPU-loop cost rather than assuming parity with Vulkan or DX12.
Do not require mesh shaders or 64-bit atomics.

Reserve complete coarse coverage before admitting/refining a cluster object.
When command or selection capacity is exhausted, replace whole valid groups
with a drawable coarse cut or retain verified conventional coverage. Never
truncate a command list and drop geometry. If even the minimum cut cannot fit,
do not transfer ownership from the fallback route; report the unmet capacity.
Reset draw arguments, enforce bounds and reject stale world/resource
generations before submission. Inspect overflow and fallback diagnostics.

First prove selection correctness and overhead against conventional LOD.
Then add conservative frustum/HZB culling and compatible material batching.
Occlusion-history invalidation needs camera-turn, teleport and rebase tests.
Fewer triangles alone does not establish faster frames; draws, batches and
selection/submission costs may dominate.

## Later paging and spatial HLOD

Paging follows the resident experiment. Retain drawable coarse ancestors while
loading finer geometry pages, and charge disk reads, decoding, uploads,
residency and retirement to shared streaming budgets. Admit detail only after
its pages are drawable; misses, cancellation and eviction select a valid
coarser cut. Use generation-safe page identities and bounded request queues.
A resident hierarchy is not yet geometry virtualization.

Spatial HLOD is a separate experiment for distant settlements or other groups
whose batch cost remains high. Compare it with per-object detail at matched
quality. Preserve object membership, mutation escape, material boundaries,
invalidation and drawable handoff. Paging/HLOD formats and budgets remain
undecided until measured workloads justify them.

## Delivery and acceptance

| Milestone | Prerequisite | Required evidence |
|---|---|---|
| Shared object lifecycle | Terrain streaming/publication contracts; reviewed eligibility and ownership | Per-object identity, exclusions, mutation, mixed-ready handoff and generation fixtures |
| Converter comparison | Reviewed optional sidecar/fallback contract | Reproducible candidates, ABI/payload audit, source preservation, quality and conversion/cache/memory evidence |
| Resident cluster renderer | Shared lifecycle and validated payloads | Legal cuts, view coverage, malformed/stale rejection, bounded overflow fallback and Metal/Vulkan/DX12 submission |
| Culling and batching | Correct resident renderer | Conservative visibility and motion recovery, compatible batches, measured CPU/render/GPU cost |
| Paging | Accepted resident behavior and measured residency need | Coarse coverage, bounded reads/decode/uploads, cancellation, eviction and recovery |
| Spatial HLOD | Measured batch pressure and mutation ownership | Matched-quality batch savings, individual-object escape and group handoff |

Use source rendering, generated conventional LOD and cluster LOD as controls
at matched visual quality. Record exact source/build/settings/hardware
identities, startup, traversal, camera turns, teleport/recovery and long
sessions. Report FPS and frame percentiles, CPU/render-thread/GPU costs,
draws/triangles, memory, uploads and time to drawable coverage. Preserve existing
acceptance budgets; unavailable GPU counters remain unavailable. Synthetic
fixtures and static captures cannot establish whole-game or target-hardware
acceptance.

Keep converter/runtime cluster experiments, paging and HLOD opt-in until their
own category and backend gates pass. Terrain's existing defaults remain
separate. Tree, dynamic-proxy and terrain-shape acceptance do not follow from
passing opaque-static cluster checks. Decide any Bevy 0.20 migration separately
from this experiment's format and backend review.

## Independent reflection milestones

[#203](https://github.com/Mudcrab-Team/mudcrab/issues/203#issuecomment-6075563397)
reports downstream SSR gains after removing a reflection view. Those results
do not predict the first native port's gain, since it retains the planar
texture as the miss fill.

1. SSR with planar fallback: validate nearby reflections, forward/water
   integration, color/depth inputs, moving-camera artifacts and integration
   cost on main's view/layer setup.
2. SSR with parallax-corrected probes and the planar camera disabled: validate
   off-screen/hidden reflections, probe placement/freshness, fallback quality,
   capture-induced loading stalls and measured view-cost removal.

Probe captures and refresh must carry source/world identity and bounded load
work. Keep fixed cubemap/sky fallback policy explicit. Each milestone remains
setting-controlled until its visual and performance gates pass. Reflection
work neither blocks terrain/object LOD nor opens their acceptance gates.

## Proposed issue amendments for owner review

These are staged text only; the GitHub issues have not been changed.

For [#103](https://github.com/Mudcrab-Team/mudcrab/issues/103), replace the
authored-only object scope with: "After terrain's shared contracts, add
conservative static-object LOD using verified authored or automatically
generated conventional sources. Compare an opt-in meshoptimizer cluster
route from the same inputs at matched quality. Share residency, eligibility,
per-object identity, invalidation and drawable handoff; keep terrain, trees
and unsupported categories on their own routes. Paging and spatial HLOD
follow measured resident-runtime results, and reflections remain independent."

For [#106](https://github.com/Mudcrab-Team/mudcrab/issues/106), extend source
inventory to include validated automatic conventional and cluster candidates,
while retaining authored shape/material/load-order provenance. Add acceptance
for per-reference mixed-ready handoff, mutation escape, complete group cuts,
source fallback, versioned sidecar rejection, bounded command overflow and
Metal/Vulkan/DX12 comparisons. Preserve the current conservative `STAT`
eligibility and separate tree/dynamic deliveries. Missing authored variants
allow verified generation; missing or unsuitable source assets remain explicit
omissions. These additions require review before the experiment broadens.

## Offline control export implemented

The opt-in `--export-controls` experiment now writes whole-model source,
conventional and complete uniform cluster-cut GLBs from the same admitted
primitives. It preserves source vertex bytes, accessor layouts, node transforms,
materials and scene ownership. Omitted primitives retain their source geometry;
local images are embedded, including dependencies used only by retained
primitives. The manifest hashes each control and records retained coverage,
per-primitive triangle counts and selected terminal depths.

Independent decoding verifies index selection and source payload/metadata
preservation. This provides renderable controls for the next comparison gate.
It does not implement adaptive view cuts, a runtime sidecar ABI, paging or HLOD,
and does not certify conservative error or rendered quality. Vanilla and modded
asset sets still need independent provenance, identical camera/material
settings, and matched-quality visual and timing comparisons before promotion.
