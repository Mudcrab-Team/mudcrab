# Skyrim's native room culling and Mudcrab parity

Research date: 2026-11-15. Mudcrab baseline: `8624434`.

## Overview

For the current implementation and the later REA trace of exclusion predicates, portal
visitation, and selective complexity fallback, see [remaining parity gaps from REA](skyrim-culling-rea-parity.md).

Decompilation of the installed SkyrimSE 1.7.104.0 executable establishes that Skyrim retains
**per-node visibility volumes and consumes them during object culling**. Mudcrab at baseline
`8624434` discards portal regions after traversal and retains room booleans. This is a demonstrated
implementation difference. No controlled comparison against a running Skyrim instance was
performed, so this research does not establish a particular visible rendering defect.

The original engine also traverses unbounded space, combines visibility for portal-shared
geometry, and tests joined room bounds. Its visibility expressions include occluder exclusions.
An equivalent implementation can use unions and intersections of clipped volumes without
reproducing Skyrim's operator interpreter.

This report concerns the pinned Special Edition executable. It does not establish that the
2011 32-bit executable has identical algorithms or numeric tolerances.

## Evidence and address coordinates

The local executable has SHA-256
`846efccf0c1374d71f892907f46549560f2fcb0a75cb87a3eed438baa0f1402f`.
Its preferred image base is `0x140000000`. All function addresses in this report are **RVAs**.
For example, RVA `0xfd96a0` has preferred VA `0x140fd96a0`. ASLR changes the loaded base.
RVAs are not file offsets.

The devenv research environment uses Ghidra 12.2, Java 25, LLVM 20.1.8, and
BethesdaGhidraScripts v1.2.3. The installed CommonLibSSE checkout is
`9d0e483f869cc7e82d5fe7e36939a6a3f9380163`. Selected newer wrappers were checked at
[powerof3/CommonLibSSE `654e55f`](https://github.com/powerof3/CommonLibSSE/tree/654e55f7979c1733033d5dbef176e43827bac944).
These sources supply community names and layouts. Native instruction behavior supplies the
algorithmic evidence below.

The exact-version Address Library file has SHA-256
`8aab3dd251d135b849bd983f86a4a205c920fa3e81f8e30c0e63ccfef9423842`.
It identifies `SkyrimSE.exe` version `1.7.104.0` and maps AE IDs to RVAs.
RTTI checks independently confirmed the primary vtables of `BSCullingProcess`,
`BSMultiBoundRoom`, and `BSMultiBoundNode`, with Complete Object Locator offset zero.
The mapping explorer also checked `BSPortalSharedNode` and the graph classes.

The public [awarebayes research checkout at `3c83806`](https://github.com/awarebayes/skyrim-research/tree/3c83806196a71329248edd7c00060461dff91a80)
matches the executable hash. It provides useful anchors, but its mapped visibility helpers
and reconstructed leaf functions do not independently establish portal traversal.
One community name-table destructor association disagrees with the RTTI-confirmed destructor.
Names alone were therefore insufficient evidence.

Local evidence is under the research environment's `.devenv/state/skyrim-culling/` directory.
The saved batches are `initial-decomp.json`, `walk-decomp.json`, `traversal-decomp.json`,
`plane-helpers.json`, `assignment-decomp.json`, and `assignment-helpers.json`.
The checked-in `research/culling-seeds.json` and `scripts/culling_trace.py` in the local
`skyrim-research-tools` checkout regenerate a combined `native-decomp.json` from the
owned executable. Generated projects, game binaries, and raw decompilation remain local.

## How it works

### Prepare visibility and select starting rooms

The main world-camera builder at RVA `0x1a97b0` clears the entry’s root
(`+0x30`) and accumulated rooms before testing containment. The ordinary scan reads the
graph’s room array (`+0x40`) from index zero upward. It starts a walk for every containing
room and records the first match as the root. A previous frame’s root is cleared before the
builder reads that field again. The light-owned paths at `0x1512540` and `0x1513290`
have their own preparation logic.

The containment point is camera translation plus the forward basis vector times the near
distance (`+0x160`). Fixed-eye poses in Treva’s Watch verified this direction in the main
view. The [parity notes](skyrim-culling-rea-parity.md#camera-and-portal-boundary-rules-are-more-specific-than-a-distance-constant)
describe the retail overlap test and its limits.

Room `QPointWithin`, RVA `0xfce0c0`, accepts the primary multibound or any joined multibound.
The bound classifiers at `0xfce130` and `0xfce1c0` also aggregate primary and joined shapes,
stopping at result `1`. A joined room is therefore a union of authored bounds. Geometric
overlap between unrelated boxes is insufficient evidence of an authored join.

Evidence: `traversal-decomp.json` and `walk-decomp.json`.

### Traverse portals with accumulated spatial constraints

RVA `0x15100b0` uses a queue of rooms, including a null-room state. Each room retrieves its
mapped compound frustum. Portal consideration requires an unvisited shared-node map entry,
a normal-view test unless bypassed, and a current-compound-frustum test unless clipping is
disabled. The opposite endpoint is queued. Its volume incorporates portal constraints,
incoming constraints, and destination occluders.

Existing destination volumes receive further contributions. Alternative-path union is strongly
supported by the expression-building calls and the AND/OR threading at `0xff9360`.
The exact merge-helper and revisit semantics still need confirmation. The shared-node map
prevents repeated processing of a portal, so a mathematically unrestricted path enumeration
would not automatically reproduce this walk.

One observed complexity fallback omits the incoming expression when its operator count reaches
`0x1a`, or 26. Portal and destination constraints still apply in that branch. This is an
expression-size threshold, not a maximum number of traversed portals.

Evidence: `traversal-decomp.json` and `walk-decomp.json`.

### Represent unbounded space explicitly

When no room contains the viewpoint, traversal starts at null. Null endpoints use the graph
entry's embedded frustum at `+0x68`. Processing null sets `visibleUnboundSpace` at `+0x130`
and visits graph-level portals. Preparation conditionally submits additional graph nodes when
that flag is set.

Outside rooms therefore participates in portal traversal. Starting outside every room does
not inherently request the entire cell. A null portal endpoint is a legitimate representation
of unbounded space in this runtime, even though a missing or foreign endpoint in our converted
data cannot safely be assumed to mean the same thing.

Evidence: `traversal-decomp.json` and shared-volume builder `0x1510940` in `initial-decomp.json`.

### Cull geometry through the resulting volumes

Room `OnVisible`, RVA `0xfcddd0`, and shared-node `OnVisible`, RVA `0xfe9e20`, require mapped
visibility under normal modes. They install that node's compound frustum while visiting
children, then restore the surrounding state. `BSCullingProcess::Process1`, RVA `0xfeda70`,
applies ordinary-view and compound tests and preserves active-plane state between objects.

`BSCompoundFrustum::Process`, RVA `0xff7790`, tests object bounding spheres against Boolean
combinations of plane sets. A sphere outside an inclusion plane fails. A sphere completely
inside an exclusion volume also fails. Partial overlap with an exclusion remains conservative.
The observed interpreter uses 12-byte operator records with true and false continuations.
Threading helper `0xff9360` gives AND and OR groups their control flow.

The native code therefore narrows individual objects inside a reached room. A room being
reachable is insufficient by itself to admit every object belonging to that room.

Evidence: `initial-decomp.json` and `walk-decomp.json`.

### Give shared geometry its own volume

`BuildSharedCompoundFrustum`, RVA `0x1510940`, combines visible endpoint volumes with an
explicit OR group. A null endpoint consults the embedded unbounded volume and its visibility
flag. An unrestricted visible endpoint also makes the shared result unrestricted.

`BSCullingProcess::AddShared`, RVA `0xfee7d0`, uses a pointer map to reject duplicate shared
objects. Portal-shared geometry thus has explicit visibility and deduplication behavior.
Extra overlap membership rows alone do not establish equivalent ownership.

Evidence: `initial-decomp.json`.

## Further native findings

Portal-plane builder `0xff7df0` obtains portal corners, constructs planes using the viewpoint,
and appends an inclusion test. When the absolute portal-plane distance is at most `5.0`, it
clears that new plane set's active mask. `WithinFrustumDistFirst`, RVA `0xfe94e0`, has another
near-plane branch with `-5.0 <= distance < 5.0` and an aperture containment test.
These conditions are more specific than Mudcrab's eight-unit bypass. Replacing one constant
would not reproduce them. Exact winding, facing, and boundary behavior remain open.

Occluder helpers `0xff8010` and `0xff7f50` build exclusion expressions for supported occlusion
shapes. They include plane and box paths. Our exported room graph currently has no equivalent
occluder-volume data. Evidence: `plane-helpers.json` and `assignment-helpers.json`.

### Authored joins and object assignment

Further focused traces on the same pinned executable establish the normal static-reference
paths below. Raw native decompilation remains local in `assignment-decomp.json`,
`assignment-helpers.json`, and `/tmp/pr225-assignment-traces/`.

| RVA | Observed behavior |
| --- | --- |
| `0x15b040` | Record loader reads `XRMR` as a 14-bit link count, two metadata bits, master byte at byte 2, and padding byte. The byte goes to `RoomRefData +0x20`. `XMBR` reads a FormID through `0x16a640`; zero removes extra type `0x60`. |
| `0x16ba20` | A marker is a master if room data is absent, its master byte is nonzero, or its room-link list is empty. |
| `0x16ba90`, `0x17e9b0` | Resolve direct linked masters first, then recurse through links with cycle protection. Head-inserted link lists reverse source `XLRM` order. |
| `0x301be0` | Creates a master room and installs reciprocal links to its linked markers. |
| `0x2da3d0` | Appends a slave multibound to the master's joined list at `+0x168`; it does not create an independent room node. |
| `0x2d77d0` | Portal setup resolves slave endpoints through their master before attaching them. |
| `0x2d8740` | Initial attachment to a supplied multibound node bypasses ordinary reassignment when its `AsMultiBoundNode` virtual succeeds. Thus a valid named target can own geometry outside its bounds. |
| `0x2ca230`, `0x2c9b90` | Normal reassignment prefers the authored target, then its portal neighbors, then other room candidates. Tests begin with render-node bound center `+0xe4`, radius `+0xf0`. A containing primary/joined room with zero touched portals owns the object; one physical portal uses its shared node; multiple portals use the common multibound node. Outside geometry scans graph portals, then uses the unbounded node. |
| `0x2d55d0` | An update caller invokes normal reassignment with arguments `(cell, reference, 1, 0)`. This is separate from initial attachment. |
| `0xee2920`, `0xefeca0` | Node world bounds skip zero-radius children and merge nonzero child spheres in child order. The enclosing-sphere merge retains containment cases and has a `1e-6` center-update threshold. |

An `ExtraOcclusionShape` can substitute its translation for the bound-center containment point;
this and forced/actor paths are not implemented by the normal mesh assignment port. Native room
candidate iteration uses runtime handles/hash storage, not ascending persistent FormIDs. When
several distinct masters contain a bound center, a deterministic single-owner choice from the
export alone would invent native order. Mudcrab retains all possible owners instead.

The local all-plugins scan found 4,246 box rooms, 2,444 links, and 817 explicit master bytes.
There are 831 portal endpoint occurrences naming slaves. Components with several explicitly
marked masters exist and must not be collapsed by connected-component union. Fourteen links
lack a mirror. Creating a master can add a missing reverse link; where several inferred incoming
masters depend on unknown construction order, Mudcrab retains full rendering.

The portal/sphere leaf at `0xfe9860` was checked in machine instructions, including the `cosf`
import thunk. For absolute portal-frame coordinates `(x, y, z)`, normal coordinate `y`, radius
`r > 0`, and half-width/height `(w, h)`, it rejects `x-r > w`, `z-r > h`, or `y-r > 0`.
It then computes `inset = cosf(abs(y-r)/r) * r` and accepts only when `x-inset <= w` and
`y-inset <= h`. The last comparison uses the normal coordinate, despite its unusual appearance.
An AABB/rectangle overlap or textbook sphere/rectangle-distance replacement changes this result.

[The local instruction probe](../../scripts/research/skyrim_portal_sphere_probe.py) extracts only
this function from an owned hash-pinned executable into executable memory. A small `ms_abi`
bridge supplies host `cosf` and the absolute-value constant. It executed 1,617 sampled cases with
zero mismatches against the port's formula. [The checked-in corpus](../../crates/engine/tests/fixtures/portal-sphere-native-1.7.104.json)
retains 36 boundary cases, also checked by the Rust test. This is an instruction-level experiment,
not a running-game comparison; host `cosf` need not match the Windows library at every last bit.
No extracted native instructions or assets are distributed.

## What Mudcrab needs for parity

The priorities below identify the next implementation and research work.

| Priority | Native behavior | Mudcrab at `8624434` | Required change |
| --- | --- | --- | --- |
| 1 | Node visibility volumes survive traversal and constrain child bounds. | Portal screen regions become room booleans. Every eligible mesh in a reached membership passes this stage. | Retain volume expressions per room and test current world bounds during per-view mesh filtering. Preserve contributions from separate paths. |
| 2 | Unbounded space is a traversal state with its own volume. | A viewpoint outside rooms or a reached unresolved endpoint draws the whole cell. | Represent outside space explicitly. Preserve the distinction between authored unbounded endpoints and invalid conversion data. |
| 3 | Portal-shared nodes use the OR of visible endpoint volumes and deduplicate objects. | Conservative overlap rows can keep a reference visible. | Export and retain shared ownership, then evaluate the combined endpoint volume. |
| 4 | Authored joined bounds and occluders participate in containment and clipping. | Room boxes, explicit links, and heuristic touching-box edges approximate connectivity. | Preserve authored joins and supported shapes. Export occluders and evaluate exclusion volumes. Replace heuristics only after comparison. |
| 5 | Assignment reads authored overrides and classifies reference geometry. | All flagged overrides and broad movable categories remain exempt. Membership is static. | Finish override and reassignment traces, then implement named targets and update membership when required. Preserve the existing `NONE` protection. |
| 6 | Preparation and culling have multiple modes and caller paths. | Main-view mesh filtering leaves lights and shadow lists independent. | Trace pass ownership, room lights, shadow behavior, and always-render children before claiming parity across passes. |

Mudcrab already starts traversal from all containing rooms. Preserve that behavior.
The interior loader visits the physical slots of its live cell reference set. The same stock assets produced different room arrays across loads. Capture-specific replay uses the ordered room FormIDs. See the [load-order evidence](skyrim-culling-rea-parity.md).
The code comparison is in [room traversal](../../crates/engine/src/room_culling.rs),
[membership conversion](../../crates/converter/src/esm/rooms.rs), and
[spawned reference policy](../../crates/engine/src/streaming.rs).

## Limits and validation still needed

Whole-image autoanalysis exhausted the JVM heap. Focused import and native decompilation worked,
but automatic function-body metadata and xrefs were incomplete. Some functions report a body
size of one despite producing valid decompilation. Missing callers or xrefs are not evidence
that a path is unused. Direct-call candidates came from scanning PE executable sections for
relative calls, following chained unwind records, and inspecting the candidate functions.
The scan can also encounter opcode bytes inside other instructions. Each semantic claim above
depends on the inspected native code, not the raw scan alone.

Decompiler names and signatures are provisional. Several routines have omitted arguments or
incorrect inferred return types. RTTI and public wrappers establish identities where stated.
The data flow and field accesses establish the behavioral findings.

A controlled original-game comparison is still needed for exact portal planes, path merging,
cycles, near-boundary cases, assignment updates, and pass ownership. The installed free-camera
framework can help repeat poses. Its presence alone does not record room visibility or prove
the plugin loaded successfully.

The useful runtime comparison records camera pose and FOV, containing rooms in graph order, the selected root,
unbounded visibility, per-node expressions, and submitted reference IDs at matching frames.
Image comparisons then check visible output. Cases need to include an outside-room viewpoint,
a null endpoint, overlapping starts, a joined room, multiple portal paths, shared geometry,
an occluder, a named override, `NONE`, and a moving object crossing a room boundary.

The [existing policy report](skyrim-room-culling.md) records converter evidence and Mudcrab's
earlier verification. Those captures compare Mudcrab with its own full-render control. They
do not substitute for an original-runtime comparison.

## Implementation after this research

The first implementation replaces room booleans with unions of convex portal volumes and
tests each eligible mesh's transformed AABB against its membership volumes. It preserves
separate paths and clips portal polygons for perspective views. Orthographic views keep full
rendering in interiors with rooms.
The native sphere test and our transformed AABB test can retain different conservative false
positives, so identical submitted-object lists are not guaranteed by this implementation.
The [current policy report](skyrim-room-culling.md#runtime-choices) describes the work limits,
fallbacks, and exemptions. The table above remains a comparison against baseline `8624434`.
The second implementation adds an unbounded traversal node, source-verified outside endpoints,
and combined endpoint volumes for portal-shared geometry. The authored-assignment update replaces
its approximate aggregate-AABB ownership for newly converted graphs. It resolves master/slave
unions, redirects portal endpoints, retains source NIF spheres, ports the native sphere/portal
leaf, honors initial named targets, and updates supported ownership after reference movement.
Legacy graphs retain their previous behavior. The authored path removes heuristic overlap edges
and portal-less-room fallbacks, while incomplete graphs still retain full rendering.

Remaining assignment limits include native candidate/hash order, named-target neighbor precedence,
unsupported/animated source bounds, primitive overrides, and forced/actor paths. Independent static
plane and box occluders now have export and path-local exclusion support; see the
[implementation contract](skyrim-culling-rea-parity.md#independent-occluder-implementation).
Retail captures establish independent shadows for [isolated linked plane references](skyrim-culling-rea-parity.md#linked-plane-references-in-retail). Runtime shapes with populated partner pointers, stateful occluders, clipping tolerances, lights, shadows, and always-render pass ownership remain separate
parity work. These implementations do not establish full original-runtime parity.
