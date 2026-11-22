# Skyrim room visibility and the default engine policy

The engine uses the converted room graph automatically, without an enable flag. Room culling
is on by default; the Graphics settings panel can disable it immediately and save that preference.
Missing tables, unread graphs and incomplete traversal data leave extra geometry drawn.
Room culling requires a positive completeness row for the current cell. Until a full export
verifies its portal graph, the engine keeps the cell's entire main-camera mesh list.
This policy uses Skyrim's authored room and portal records, but does not claim exact replication
of Skyrim's traversal.

Portal order can change which meshes appear downstream even with a single starting room.
Mudcrab can replay a measured load’s order. Ordinary packs use deterministic FormID order.
See the [two retail renders and comparison procedure](skyrim-culling-rea-parity.md#portal-order-within-one-starting-room-changes-the-image).

## Evidence

Research checked on 2026-11-15:

- [The converter's record analysis](interior-room-graph-converter.md) describes the `RoomMarker`,
  `PortalMarker`, `XPOD` room pairs and `XLRM` links measured in the game data. Portals identify
  apertures between rooms. The original projection used pivot and bounds-overlap membership;
  native research now identifies `XLRM` master/slave unions and source-sphere ownership.
- [Native SkyrimSE culling research](skyrim-native-culling.md) now traces the installed
  1.7.104.0 executable. It establishes portal traversal with per-node compound frusta,
  object-bound tests within reached rooms, explicit unbounded-space traversal, shared-node
  volume unions, joined bounds, and occluder exclusions. That report records exact-build RVAs,
  local evidence, the parity priorities, and remaining uncertainty.
- [awarebayes/skyrim-research at `3c83806`](https://github.com/awarebayes/skyrim-research/tree/3c83806196a71329248edd7c00060461dff91a80)
  contains room/portal declarations and reconstructed leaf functions. Its
  [BSPortalGraph implementation](https://github.com/awarebayes/skyrim-research/blob/3c83806196a71329248edd7c00060461dff91a80/reconstruction/bsengine/BSPortalGraph.cpp)
  compares a deleting destructor. `BSMultiBoundRoom` compares serialization/clone leaves, not a
  visibility walk. `BSPortalGraphEntry::ClearVisibility` and `AddVisibilityToMap` are only **mapped**
  in its [function index](https://github.com/awarebayes/skyrim-research/blob/3c83806196a71329248edd7c00060461dff91a80/reconstruction/INDEX.md).
  Those names identify useful next research targets. They do not establish traversal semantics.
- [CommonLibSSE-NG's BSPortalGraphEntry declaration](https://github.com/alandtse/CommonLibSSE-NG/blob/786b514953853a9a75bd8fe990d1e6b977d7809d/include/RE/B/BSPortalGraphEntry.h)
  has an accumulated room list, a root room, a node-to-compound-frustum visibility map, and a
  `visibleUnboundSpace` flag. Its
  [BSCompoundFrustum](https://github.com/alandtse/CommonLibSSE-NG/blob/786b514953853a9a75bd8fe990d1e6b977d7809d/include/RE/B/BSCompoundFrustum.h)
  stores frustum planes and an operator stream with union, intersection, inside and intersection
  tests. [BSCullingProcess](https://github.com/alandtse/CommonLibSSE-NG/blob/786b514953853a9a75bd8fe990d1e6b977d7809d/include/RE/B/BSCullingProcess.h)
  connects the portal graph entry, compound frustum and a shared-room object map. These community
  declarations support a per-view volume-based visibility model with unbounded-space state.
  They do not reveal how portal paths build those volumes or when individual tests run.
- Bethesda's [Skyrim optimization tutorial, revision 7673](https://ck.uesp.net/w/index.php?title=Bethesda_Tutorial_Optimization&oldid=7673)
  was verified from the user-supplied saved HTML, **Bethesda Tutorial Optimization - the CreationKit
  Wiki.html**. The page identifies itself as the UESP copy of the original Skyrim CK wiki and gives
  a last-edit date of 2017-01-27. The saved HTML SHA-256 is
  `cec90f5618ec85e664cb74cd7627be3d0328152cce217a3b7a4df63d53ba7cdc`.
  It confirms that rooms collect references for visibility, a visible portal exposes its connected
  rooms, and a working portal must link both rooms. Its advanced tips specify default membership
  in one room by the reference pivot, joined rooms treated as one irregular room, and the `NONE`
  multibound override for references that must remain visible across rooms. It warns that gaps and
  overlaps can cause missing geometry. It does **not** say that overlapping boxes automatically
  connect rooms.
- Bethesda's [Skyrim room-basics page, revision 15028](https://ck.uesp.net/w/index.php?title=Room_Bounds_and_Portal_Basics&oldid=15028)
  was verified from the user-supplied saved HTML, **Room Bounds and Portal Basics - the CreationKit
  Wiki.html**, last edited 2011-12-02. The saved HTML SHA-256 is
  `90a438d8f064ded2fb9efc653998ca0ab95b402d43830f05ecfc773d42612278`.
  It documents that a viewpoint outside room bounds can see their contents only through a portal.
  Walking into an adjacent room does not make that room visible before entry if no working portal
  connects it. It also warns about disappearing dropped objects, misplaced object pivots, and
  overlapping or separated room bounds. Its performance section attributes overhead to many
  visible portals and their frustum operations. The page recommends `MBC` to toggle multibound
  culling and `TCL` to inspect geometry. The later 1.7.104.0 runtime experiment used `TCL`, but
  found that this executable's `MBC` callback returns success without changing multibound state.
- Bethesda's [optimization tutorial for Fallout](https://geckwiki.com/index.php/Bethsoft_Tutorial_Optimization)
  additionally names a separate unbounded space connected through portals. That terminology and
  its exact traversal rules are not established by the supplied Skyrim pages. Skyrim's outside-
  bounds portal rule is documented directly above. The later native trace independently
  establishes explicit unbounded-space traversal in this SkyrimSE build.

Skyrim's native compound-frustum interpreter evaluates unions and intersections of plane sets,
including occluder exclusions. Our runtime now retains unions of convex portal volumes and tests
individual transformed mesh bounds against them. Separate paths retain their apertures rather
than filling the screen rectangle between them. Versioned graphs also traverse unbounded space
and test portal-shared geometry against both endpoint volumes. Source-verified independent plane
and box occluders now contribute path-local exclusions, tested with conservative enclosing spheres;
unresolved connections still draw the whole cell.

No reconstruction source was copied into the engine. A later
[four-pose original-game comparison](skyrim-original-comparison.md) supports the broad visibility
decisions in three disputed shell views and retains the joined-room cave. It does not establish
object-by-object or whole-engine parity. The native trace establishes how room volumes reach object culling and how
unbounded space enters traversal. Exact clipping tolerances, path merging and revisits, full
specialized assignment paths, actor updates, lights, and shadows still need further tracing
or controlled Skyrim experiments.

## Differences established by the Skyrim tutorial

The tutorial documents authored room grouping and portal connectivity, not the exact clipping
algorithm. Its pivot description guided the original geometric projection. The later pinned SE
trace identifies render-bound centers, authored master/slave grouping, and portal/sphere ownership;
new authored graphs use those findings. Pivot/overlap rows and implicit edges remain legacy policies.

The converter and runtime honor the placed-reference `Multibound` header flag (`0x80000000`).
[xEdit's TES5 definitions at `3ccc9e7`](https://github.com/TES5Edit/TES5Edit/blob/3ccc9e7d733becfcc97940cf0c877fa3d6213c5b/Core/wbDefinitionsTES5.pas)
name bit 31 as `Multibound`, identify `XMBR` as the multibound reference, and identify `XMBP` as a
primitive marker. CommonLibSSE-NG independently declares `TESObjectREFR::RecordFlags::kMultibound`
as bit 31. `NONE` must not be inferred from an absent `XMBR` alone: ordinary automatic references
also omit that field.

Legacy graphs keep all flagged references visible. The authored-assignment path instead records
remapped named targets separately from `NONE`. Initial attachment honors a valid target directly;
later supported reference movement rechecks geometric ownership. `NONE` remains visible. Export
still omits flagged references from the legacy pivot/overlap rows, and integration removes stale
rows transactionally. Databases predating header-flag export need reconversion to recover overrides.

A read-only scan of the available converted Skyrim data found 220 flagged references: 215 with
an `XMBR` target and five without one. Three untargeted references are static Katariah floor/mast
pieces (`000BBF85`, `000BBFA3`, `000C3E42`), each incorrectly assigned one containing room by the old
converter. The other two are water activators, already protected by the activator exemption.
The flag/target distribution supports the `NONE` interpretation; it is not a controlled CK edit
or a reconstruction of Skyrim's assignment code.

The room-basics page describes Skyrim restricting views from outside room bounds to portals.
Complete authored graphs start in unbounded space when no room contains the near-shifted lookup
point. Both traversal directions use source-verified outside endpoints. Legacy NULL endpoints
cannot distinguish outside space from invalid links, so legacy gaps retain full rendering.
Unknown references remain drawn. The [Gallows Rock replay](skyrim-culling-rea-parity.md#gallows-rock-west-doorway)
checks this rule against the stock interior.

Its dropped-object warning also shows why a reference's original placement is insufficient for
moving objects. Actors and animated categories remain exempt. Supported loose objects now
reclassify their source bound after their root transform changes; legacy graphs keep the exemption.
The page alone does not establish native movement timing; the later assignment trace supplies
separate initial-attachment and update paths.

The tutorial recommends measuring performance and limiting long chains of visible portals. Its
3–5-portal guidance is an authoring recommendation, not permission to truncate the renderer's walk.

## Runtime choices

A perspective view starts from all containing authored room unions with the camera frustum. Portal polygons
are clipped against incoming convex volumes; aperture edges and the doorway plane narrow the next
volume. Separate paths remain a union. Orthographic views retain full rendering in interiors with rooms
because their near plane can span multiple disconnected rooms. On a newly reached bounded room, an incoming
program of 26 or more native operator records drops its inherited constraints after the exit passes its aperture test.
The exit and the room’s own occluders still constrain its meshes. See the
[retail threshold captures](skyrim-culling-rea-parity.md#the-26-operator-fallback-retains-local-constraints).
Independent work limits can also widen a complex volume. Authored graphs allow at least one contribution per physical portal in the cell before applying that limit.

Full source conversions publish `occlusion_version = 1` with authored plane and box geometry.
Supported independent occluders add exclusions to their containing joined room or unbounded space.
Exclusions propagate with each path; an alternative clear path retains the mesh. Rejection requires
full containment of the mesh AABB's enclosing sphere. Budget widening retains local exclusions.
An explicitly targeted plane can hide meshes when the converted source confirms its unique containing room as the target. The loader treats an isolated, source-verified pair of linked planes with the same owner as two independent occluders. Other overrides, complex links, stateful primitives, and ambiguous assignments remain inactive. Older exports
retain their previous behavior until reconverted. See the [occluder implementation contract](skyrim-culling-rea-parity.md#independent-occluder-implementation).

An interior with no authored rooms can apply verified independent occluders directly to the full
camera volume. In an interior with rooms, occluders do not establish portal coverage: older packs without authored assignment and complete endpoint data retain full rendering when the lookup point falls outside every room.

Graphs with `visibility_version = 2` have explicit unbounded space. Only endpoints verified as zero
in source `XPOD` name it. Missing, invalid, or foreign-cell links remain unknown and trigger full
rendering. Room-to-outside traversal narrows its volume through the same aperture tests. When the near-shifted camera point falls outside every authored room union in a complete graph, traversal starts in unbounded space and follows its authored portals. Older conversions retain their outside-camera fallback.

New conversions additionally publish `assignment_version = 1`, ordered `XLRM` links, per-reference
rules, source NIF spheres, and per-cell completeness. `XRMR[2]` selects explicit masters. Slaves
resolve direct masters before recursive links, in native list order, with reciprocal master links
and cycle handling. A room is the union of its primary and joined boxes, not their enclosing AABB.
Portals naming slaves redirect to masters. Distinct explicit masters stay distinct. Incomplete cells
and unresolved/ambiguous master construction keep full rendering. The unique authored walk omits implicit
overlap edges and the portal-less-room heuristic. Duplicate portal rows select the legacy walk,
which keeps the portal-less-room fallback.

Supported model bounds retain native source spheres and ordered child merges. They are not rebuilt
from render AABBs. A reference's current cell-local transform places that sphere; its center selects
containing master rooms. Initial named targets override geometric containment without requiring
a source sphere. Later transform changes
recompute ownership, including after a moving object returns to its spawn pose. Missing bounds remain visible during automatic assignment and updates. `NONE`, unknown
targets, nonuniform or sheared assignment transforms, actors, and doors stay visible.
Activators with supported source bounds now use authored assignment. The
[geometry-mismatch investigation](skyrim-culling-mismatches.md) verifies the web and foliage fixes
against live native bounds and matched camera captures.
Native overlapping-room hash order cannot be reconstructed from persistent IDs, so possible owners
are combined. Named-target neighbor priority and specialized native paths remain conservative limits.

The native sphere/portal test distinguishes no touched portal, one physical portal, and several
portals. These select ordinary room/unbounded ownership, one portal's combined endpoint volumes, or
the common multibound node respectively. Multiple `XPOD` rows of one marker remain one physical
portal. Each child mesh tests its current transformed AABB against the selected owner volumes;
no duplicate render submissions are inserted. AABBs support rotation, negative/nonuniform scale,
and shear even though native assignment transforms require a uniform scale. Missing mesh bounds
skip aperture filtering but retain the unreached-space test.

With geometric membership, a mesh can use either endpoint’s visible volume when its reference
bounds intersect a doorway attached to a containing or overlapping room.
In roomed cells, empty geometric membership leaves the mesh eligible for ordinary view culling.
Authored automatic assignment to unbounded space requires a usable model sphere.

In the converted Bards College interior (cell `00016A0C`), static `000F717E` has no
geometric membership or multibound override. The pack contains eight rooms and nine portals,
all between rooms. The authored loader classifies the static as unbounded using the source
sphere for `SMDAFloSol04` (center at the model origin, radius 181.02). This is a pack and
loader check. Visibility of this floor piece at the same viewpoint in Skyrim remains unverified.

Legacy graphs retain pivot/overlap candidates, conservative box/portal sharing, open links,
touching-box edges that preserve the incoming visibility volume, and reached-portal-less-room fallbacks. Loose items and authored overrides retain
legacy exemptions. The normal converter refreshes both room metadata and source-sphere GLBs; no
feature flag or global database schema bump is added. Lights and shadow lists remain independent.

## Verification

Use the standard engine invocation with `--shots crates/engine/tests/fixtures/room-culling-shots.json`
and `--shots-out <directory>`. `scripts/room_culling_counts.py <directory>` reports the captured
frame's mesh lists before and after room culling.

For a full-render control with the same executable, use a private copy of the converted assets
whose database lacks the four optional room tables. Keep geometry and all other database content
identical. Compare repeated, settled captures at the same poses, including a portal-less hall and
an exterior return. This exercises the actual missing-table fallback without adding a product
flag. Mesh-list reduction alone does not establish image parity or an FPS improvement.

[The focused volume poses](../../crates/engine/tests/fixtures/room-volume-shots.json) cover the
portal apertures, open-hall fallbacks, a fully portal-bearing cell, and the exterior return used
to verify the retained volumes and mesh-bound filter. Capture three alternating repetitions of
each mode. Reject timed-out captures and frames with pending or failed room-graph reads.

The volume update passed 1,421 engine library tests, with seven ignored. Its new system regression
first failed because a reached room retained a mesh outside the aperture. The passing test also
covers transformed cell roots, separate camera volumes, and explicit frustum opt-outs. The real
visibility-schedule fixture now verifies transform propagation before testing mesh bounds.

[The native validation record](room-visibility-volumes-20261115.json) records 36 sequence captures
and six additional fresh-process fallback captures at 1400x1050 on Vulkan. Across all nine
cross-mode image pairings, the four portal-culling poses and exterior return have zero persistent
differing pixels at RGB threshold 8. The unculled open hall has six differing pixels in the
sequence, with identical mesh lists in every run. Replaying that pose alone in fresh processes
has zero persistent differing pixels. This supports a capture-history explanation for the small
sequence difference rather than an aperture rejection. No capture timed out or retained pending
or failed room-graph reads. The record retains this distinction rather than reporting every
sequence image as identical.

The unbounded-space update passed 1,427 engine library tests, 1,176 converter library tests,
and 32 plugin-reference integration tests; seven, 30, and one were ignored respectively.
The shared-geometry system regression first failed with a child mesh hidden on the near side
of its owner's portal. It now passes using the combined endpoint volumes. Tests also cover
outside starts, room-to-outside aperture rejection, legacy migration, and source null provenance
through the complete typed conversion path.

[The unbounded validation record](room-unbounded-visibility-20261115.json) retains 78 native
captures. All 36 captures at the six existing poses have zero persistent differing pixels at
threshold 8. The [four boundary poses](../../crates/engine/tests/fixtures/room-unbounded-shots.json)
step across an authored outside portal in Gallows Rock, 16 units from its plane. Three views
match full rendering. Looking away from the portal into the back side of the level shell changes
384,120 pixels and reduces the mesh list from 149 to 15. That view exercises the new restriction
but is not an image-equality pass or an original-game oracle. An earlier Shimmermist probe also
looked through backfaces outside the cave shell; its differing image and two blank views remain
recorded as exploratory results. No capture timed out or retained pending or failed graph reads.


The authored-assignment update passed 1,436 engine, 1,177 converter, 91 shared, and 32
plugin-reference tests (seven, 30, zero, and one ignored), plus workspace Clippy with warnings
denied. The source-sphere conversion test distinguishes NIF spheres from render bounds and checks
child transforms and the ignored placement root. Runtime tests cover disjoint joined bounds,
slave portal redirection, named initial attachment versus later updates, `NONE`, moving loose
references, and independently moving ancestors. Typed publication preserves master bytes, link
order, and named/NONE/automatic rules. The native instruction probe checks 1,617 sampled cases,
with 36 saved boundary cases also tested in Rust.

[The authored-assignment validation record](room-authored-assignment-20261115.json) retains 78
Mudcrab captures from November 15, before the later source-bounds fixes. That build used a
private four-cell dataset with 989 converted models, 860 source-sphere GLBs, and 2,799 base
records with source spheres. The main comparison has 72 captures at
12 poses, three alternating repetitions per mode, and all nine cross-mode image pairings. Eight
views have zero persistent differing pixels at threshold 8. Abandoned Prison has one sequence
pixel; six additional fresh-process captures match at that pose, supporting a capture-history
explanation while retaining the sequence result.

Three shell-facing views do not match full rendering: the old tall-box-center Bleak Falls Barrow
pose changes 320,661 pixels, Gallows Rock outside-away changes 366,348, and its box-inside/outward
view changes 207,106. These historical equality failures were recorded before a retail oracle
was available. The [2026-11-17 retail comparison](room-assignment-review-20261117.md) now shows
that Skyrim also omits the large background shells at the Bleak Falls and inside-out Gallows
poses. Fog, lighting, props, and material sampling still differ. At `b3a1793e`, the outside-away pose kept full rendering. With a complete reconverted graph,
the camera now traverses unbounded space and retains the five meshes of the stair arch.
See the [Gallows Rock seam replay](skyrim-culling-rea-parity.md#gallows-rock-west-doorway).
The two new [joined-room views](../../crates/engine/tests/fixtures/room-assignment-shots.json),
inside the master and slave cave geometry, match in every pairing while exercising culling.
No final capture timed out or retained pending/failed room-graph reads. These results support the
covered behavior, not a claim that every interior matches Skyrim.

For an existing source-verified pack, the `room_assignment_validation` converter example refreshes
the source meshes used by the selected cells and finalizes their bounds in an independent private
copy. It requires both verified portal endpoints and authored room assignments. It retains the
source graph for every cell, so later runs can expand the selection without losing rejected links
or unknown assignments. The helper uses the installed official archives, requires Unix hard-link
validation, and holds the asset directory's exclusive lock through finalization. Use the standard
conversion pipeline to rebuild room metadata from plugins.

## Default-on review validation

[The 2026-11-17 core review record](room-culling-review-20261117.md) contains the graphics toggle,
three real-interior on/off pairs with counts, repeated release frame times, executable identities,
and their limits. The core remains a room-reachability foundation, not a complete Skyrim parity claim.

[The boundary review record](room-gap-review-20261117.md) covers conservative camera starts,
negative extents, legacy-pack handling, and the sampled out-and-back room-boundary sequence.

[The authored-assignment review record](room-assignment-review-20261117.md) adds matched retail
images for the disputed shells and a fresh named-target/NONE capture from Katariah.
