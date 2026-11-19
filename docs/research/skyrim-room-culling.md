# Skyrim room visibility and the default engine policy

The engine uses the converted room graph automatically, without an enable flag. Room culling
is on by default; the Graphics settings panel can disable it immediately and save that preference.
Missing tables, unread graphs and incomplete traversal data leave extra geometry drawn.
An older pack without per-cell completeness records can hide rooms behind a portal its exporter
skipped. A full rebuild flags that cell so the engine keeps its meshes visible.
This policy uses Skyrim's authored room and portal records, but does not claim exact replication
of Skyrim's traversal.

## Evidence

Research checked on 2026-11-15:

- [The converter's record analysis](interior-room-graph-converter.md) describes the `RoomMarker`,
  `PortalMarker`, `XPOD` room pairs and `XLRM` links measured in the game data. Portals identify
  apertures between rooms. Links connect rooms that must be seen together without an aperture.
  Membership in our conversion uses the reference pivot, with additional bounds-overlap rows.
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
  culling and `TCL` to inspect geometry, but those commands were not exercised in Skyrim here.
- Bethesda's [optimization tutorial for Fallout](https://geckwiki.com/index.php/Bethsoft_Tutorial_Optimization)
  additionally names a separate unbounded space connected through portals. That terminology and
  its exact traversal rules are not established by the supplied Skyrim pages. Skyrim's outside-
  bounds portal rule is documented directly above. The later native trace independently
  establishes explicit unbounded-space traversal in this SkyrimSE build.

Skyrim's native compound-frustum interpreter evaluates unions and intersections of plane sets,
including occluder exclusions. Our runtime now retains unions of convex portal volumes and tests
individual transformed mesh bounds against them. Separate paths retain their apertures rather
than filling the screen rectangle between them. Versioned graphs also traverse unbounded space
and test portal-shared geometry against both endpoint volumes. Occluder exclusions remain absent;
unresolved connections still draw the whole cell.

No reconstruction source was copied into the engine. No original Skyrim runtime comparison was
performed. The native trace now establishes how room volumes reach object culling and how
unbounded space enters traversal. Exact clipping tolerances, path merging and revisits, full
assignment policy, dynamic membership updates, lights, and shadows still need further tracing
or controlled Skyrim experiments.

## Differences established by the Skyrim tutorial

The tutorial documents authored room grouping and portal connectivity, not the exact clipping
algorithm. It supports pivot membership and explicit room links, while leaving our screen-space
clipping and implicit overlap edges as implementation choices.

The converter and runtime honor the placed-reference `Multibound` header flag (`0x80000000`).
[xEdit's TES5 definitions at `3ccc9e7`](https://github.com/TES5Edit/TES5Edit/blob/3ccc9e7d733becfcc97940cf0c877fa3d6213c5b/Core/wbDefinitionsTES5.pas)
name bit 31 as `Multibound`, identify `XMBR` as the multibound reference, and identify `XMBP` as a
primitive marker. CommonLibSSE-NG independently declares `TESObjectREFR::RecordFlags::kMultibound`
as bit 31. `NONE` must not be inferred from an absent `XMBR` alone: ordinary automatic references
also omit that field.

The implementation keeps every flagged reference visible rather than assigning it to geometric
room boxes. This preserves `NONE` and conservatively draws references with named authored targets
too; exact named-target culling is not implemented. It does not interpret the unremapped `XMBR`
FormID. Export omits these references from pivot membership, and overlap integration removes stale
membership rows and excludes the references when rebuilding overlaps. Integration does this in
one transaction and can be repeated.

The database loader carries the already-exported header bit through `ReferenceRow` to the spawned
root's `RoomCullExempt` component. Consequently existing conversions that retained `header_flags`
are protected even while their `reference_rooms` table still contains old geometric membership.
Databases predating header-flag export cannot recover information that was never stored and need
reconversion to recover authored overrides. No world schema bump or enable flag is added.

A read-only scan of the available converted Skyrim data found 220 flagged references: 215 with
an `XMBR` target and five without one. Three untargeted references are static Katariah floor/mast
pieces (`000BBF85`, `000BBFA3`, `000C3E42`), each incorrectly assigned one containing room by the old
converter. The other two are water activators, already protected by the activator exemption.
The flag/target distribution supports the `NONE` interpretation; it is not a controlled CK edit
or a reconstruction of Skyrim's assignment code.

The room-basics page describes Skyrim restricting views from outside room bounds to portals.
Mudcrab keeps whole-cell rendering when the camera is outside every room, including in versioned
graphs, because gaps between bounds are not verified outside space. From a containing room,
versioned graphs can traverse an authored outside endpoint through its portal aperture. Legacy
NULL endpoints cannot distinguish authored outside from invalid links. Unknown references remain
drawn. This conservative camera-start policy differs from the traced native null-room traversal;
it does not change versioned object membership to draw every unassigned reference.

Its dropped-object warning also shows why a reference's original placement is insufficient for
moving objects. Our actor and movable-object exemptions deliberately avoid culling by stale
membership. The page does not establish how Skyrim updates membership as objects move or settle.

The tutorial recommends measuring performance and limiting long chains of visible portals. Its
3–5-portal guidance is an authoring recommendation, not permission to truncate the renderer's walk.

## Runtime choices

The camera's containing rooms start with the camera frustum. Each portal polygon is clipped
against its incoming volume. The aperture edges and the plane behind the doorway constrain the
next volume. Each room retains a union of these convex volumes. Explicit room links pass the
incoming volume unchanged. Perspective views use converging rays and orthographic views use
parallel rays.

Graphs marked `visibility_version = 2` have an explicit unbounded node. Room-to-outside traversal
narrows its volume through the same aperture tests. A camera outside every room keeps full
rendering: room boxes can have seams, so their complement is not verified outside-space coverage.
This conservative start policy deliberately differs from the traced native null-room traversal. Only an endpoint verified as zero in the source `XPOD` names this node.
Missing or invalid links remain unknown. The converter adds provenance columns with zero defaults
and publishes verified flags atomically, so older or interrupted conversions retain safe behavior.

For static references with one containing room, each mesh's current AABB is transformed into
cell-local space using its full affine transform. It stays drawn if it intersects any volume of
its containing or overlapping rooms. The plane support test handles rotation, nonuniform and
negative scale, and shear. A touching or straddling bound stays visible even if its center is
outside the aperture. Missing or invalid bounds and `NoFrustumCulling` skip aperture filtering
while retaining the prior unreached-room rule.

Known references with no pivot-containing room use the unbounded volume in versioned graphs.
Their bounds-overlap room memberships remain candidates too. The runtime recognizes portal-shared
geometry when a reference's aggregate model bounds overlap a portal attached to its containing
space. Each child mesh can then use either endpoint's volume, including the unbounded endpoint.
This adds visibility candidates without discarding existing overlap memberships. It filters each
mesh once and does not duplicate render submissions. Missing aggregate bounds use the mesh bounds.
The affine-box overlap test is conservative; it does not reproduce native sphere assignment,
multi-portal ownership, or authored named overrides exactly.

The walk accepts up to 16 independent contributions per room and 64 planes per volume. Exceeding
either budget widens that room to the camera frustum and propagates the wider result. These are
conservative engine work limits, not Skyrim's native operator budget. Flagged multibound
references, actors, doors, activators, and movable objects retain their exemptions. Shadows and
lights retain their independent visibility.

Some shipped open halls have room boxes without any valid portal. Their geometry can expose a
far room across space outside all the boxes. The earlier implementation drew the whole cell only
when the camera started in such a room. That is insufficient: a portal-bearing start can reach an
open corridor later, and then incorrectly hide a visible backdrop behind an off-screen portal.
The default policy therefore draws the whole cell if **any reached room** lacks a valid portal
between distinct known spaces. Any unknown portal endpoint also triggers the whole-cell fallback:
it may conceal connectivity that conversion could not recover. Authored unbounded endpoints are
known spaces and do not trigger this fallback. Regression fixtures cover these cases.

Touching or overlapping boxes still add conservative connectivity. Between rooms with portals the
walk clips to the destination box's projected rectangle. This is an engine heuristic, not verified
Skyrim behavior. A graph made entirely of portal-bearing rooms can still have coverage gaps;
synthetic tests and sampled image comparisons do not prove all interiors safe. Authored joined
bounds, exact assignment, and original-engine comparisons remain research work.

Databases without room tables draw the cell normally. Legacy graphs also draw the whole cell when
the camera is outside every room. Graph reads run asynchronously, and captures wait for them.

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

## Default-on review validation

[The 2026-11-17 core review record](room-culling-review-20261117.md) contains the graphics toggle,
three real-interior on/off pairs with counts, repeated release frame times, executable identities,
and their limits. The core remains a room-reachability foundation, not a complete Skyrim parity claim.

[The boundary review record](room-gap-review-20261117.md) covers conservative camera starts,
negative extents, legacy-pack handling, and the sampled out-and-back room-boundary sequence.
