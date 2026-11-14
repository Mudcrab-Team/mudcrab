# Skyrim room visibility and the default engine policy

The engine uses the converted room graph automatically. There is no enable flag or configuration
switch. Missing tables, unread graphs and incomplete traversal data leave extra geometry drawn.
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
- Bethesda's [optimization tutorial for Fallout](https://geckwiki.com/index.php/Bethsoft_Tutorial_Optimization)
  describes a separate unbounded space connected through portals. The supplied Skyrim tutorial
  does not describe that rule. Unbounded-space evidence for Skyrim therefore still comes from the
  community declarations above, rather than this tutorial. The separate Skyrim
  [room basics page](https://ck.uesp.net/wiki/Room_Bounds_and_Portal_Basics) remains unverified.

The structural comparison is useful: Skyrim's declared compound frusta can represent unions and
intersections of plane sets. Our screen rectangles approximate those visible regions and merge
multiple paths conservatively. We do not implement the original visibility map or unbounded-space
traversal, which is why unresolved connections draw the whole cell instead of being ignored.

No reconstruction source was copied into the engine. No original Skyrim runtime comparison was
performed. The evidence supports using authored apertures and explicit room links; the exact
handling of overlapping bounds, unbounded-space traversal, dynamic membership, lights and shadows
still needs controlled Skyrim experiments or reconstructed traversal functions.

## Differences established by the Skyrim tutorial

The tutorial documents authored room grouping and portal connectivity, not the exact clipping
algorithm. It supports pivot membership and explicit room links, while leaving our screen-space
clipping and implicit overlap edges as implementation choices.

The converter does not yet preserve the authored `NONE` multibound override. `export_room_graph`
in `crates/converter/src/esm/rooms.rs` assigns every placed non-marker reference by pivot position;
its later overlap pass adds bounds intersections. A reference assigned to `NONE` in the editor can
therefore acquire room membership here and be culled. Drawing references with several containing
rooms does not implement this override: an explicitly unrestricted reference can still have its
pivot in exactly one room. The tutorial does not identify the serialized field/value for `NONE`.
The schema names `XMBR` as a multibound field and also retains `XMBP`, but that alone does not prove
how the editor encodes the override. Confirm that representation with xEdit definitions or a
controlled CK edit before implementing it. Existing converted data would need correction too.

The tutorial recommends measuring performance and limiting long chains of visible portals. Its
3–5-portal guidance is an authoring recommendation, not permission to truncate the renderer's walk.

## Runtime choices

The camera's containing rooms start a portal walk. Projected doorway rectangles narrow the view
region along each path. Explicit room links pass the region through unchanged. Static references
with one containing room are hidden only if none of their containing or overlapping rooms is
reached. Actors, doors, activators and movable objects keep their existing exemption. Shadows and
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
