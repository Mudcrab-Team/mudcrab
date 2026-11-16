# Skyrim room visibility and the default engine policy

The engine uses the converted room graph automatically, without an enable flag. Room culling
is on by default; the Graphics settings panel can disable it immediately and save that preference.
Missing tables, unread graphs and incomplete traversal data leave extra geometry drawn.
This policy uses Skyrim's authored room and portal records, but does not claim exact replication
of Skyrim's traversal.

## Evidence

Research checked on 2026-11-15:

- [The converter's record analysis](interior-room-graph-converter.md) describes the `RoomMarker`,
  `PortalMarker`, `XPOD` room pairs and `XLRM` links measured in the game data. Portals identify
  apertures between rooms. Links connect rooms that must be seen together without an aperture.
  Membership in our conversion uses the reference pivot, with additional bounds-overlap rows.
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
  bounds portal rule is now documented directly above; the explicit unbounded-space state remains
  supported by the community declarations.

The structural comparison is useful: Skyrim's declared compound frusta can represent unions and
intersections of plane sets. Our screen rectangles approximate those visible regions and merge
multiple paths conservatively. We do not implement the original visibility map or unbounded-space
traversal, which is why unresolved connections draw the whole cell instead of being ignored.

No reconstruction source was copied into the engine. No original Skyrim runtime comparison was
performed. The evidence supports using authored apertures and explicit room links; the exact
clipping at overlapping bounds, construction of unbounded-space visibility, dynamic membership
updates, lights and shadows still needs controlled Skyrim experiments or reconstructed traversal
functions.

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

The room-basics page establishes that Skyrim restricts views from outside room bounds to portals.
Our camera-outside-all-rooms fallback instead draws the whole cell. Likewise, drawing references
with no geometric membership is deliberately more permissive than the documented pivot rule.
These choices preserve geometry when our graph lacks the original renderer's connectivity and
assignment information; they are not replicas of the documented visibility policy.

Its dropped-object warning also shows why a reference's original placement is insufficient for
moving objects. Our actor and movable-object exemptions deliberately avoid culling by stale
membership. The page does not establish how Skyrim updates membership as objects move or settle.

The tutorial recommends measuring performance and limiting long chains of visible portals. Its
3–5-portal guidance is an authoring recommendation, not permission to truncate the renderer's walk.

## Runtime choices

The camera's containing rooms start a portal walk. Projected doorway rectangles narrow the view
region along each path. Explicit room links pass the region through unchanged. Static references
with one containing room are hidden only if none of their containing or overlapping rooms is
reached. Flagged multibound references, actors, doors, activators and movable objects remain exempt. Shadows and
lights retain their independent visibility.

Some shipped open halls have room boxes without any valid portal. Their geometry can expose a
far room across space outside all the boxes. The earlier implementation drew the whole cell only
when the camera started in such a room. That is insufficient: a portal-bearing start can reach an
open corridor later, and then incorrectly hide a visible backdrop behind an off-screen portal.
The default policy therefore draws the whole cell if **any reached room** lacks a valid portal
between two distinct rooms of that cell. It also draws the whole cell when a reached room has a
portal with an unresolved side: that side may open into unbounded space, whose connectivity we do
not model. Regression fixtures cover both cases.

Touching or overlapping boxes still add conservative connectivity. Between rooms with portals the
walk clips to the destination box's projected rectangle. This is an engine heuristic, not verified
Skyrim behavior. A graph made entirely of portal-bearing rooms can still have coverage gaps;
synthetic tests and sampled image comparisons do not prove all interiors safe. A complete
unbounded-space model and original-engine comparisons remain research work.

Databases without room tables draw the cell normally. A camera outside every room also draws the
cell normally. Graph reads run asynchronously, and captures wait for them before measuring.

## Verification

Use the standard engine invocation with `--shots crates/engine/tests/fixtures/room-culling-shots.json`
and `--shots-out <directory>`. `scripts/room_culling_counts.py <directory>` reports the captured
frame's mesh lists before and after room culling.

For a full-render control with the same executable, use a private copy of the converted assets
whose database lacks the four optional room tables. Keep geometry and all other database content
identical. Compare repeated, settled captures at the same poses, including a portal-less hall and
an exterior return. This exercises the actual missing-table fallback without adding a product
flag. Mesh-list reduction alone does not establish image parity or an FPS improvement.
