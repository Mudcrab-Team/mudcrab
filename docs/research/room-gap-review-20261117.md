# Room-boundary review validation, 2026-11-17

The 2026-11-17 build kept full rendering for a camera outside every authored room. The subsequent [retail Gallows Rock capture](skyrim-culling-rea-parity.md#gallows-rock-west-doorway) supersedes that choice for complete authored graphs. Room boxes do not prove that their
complement is legitimate outside space. This avoids interpreting a seam between boxes as an
unbounded-space start. Traversal from rooms through authored outside portals remains supported.
This choice deliberately draws more than native null-room traversal.

Negative room half-extents and negative portal aperture half-extents make the affected cell
incomplete during export. The runtime also rejects these extents in older packs without new
completeness metadata. Valid neighboring cells continue to cull.

The volume layer passed 70 culling tests, 17 converter room tests, 34 reference tests with one
ignored, four snapshots, formatting, and strict engine/converter Clippy. New seam and negative-
extent regressions failed before the fix. The combined stack passed 1,640 engine tests with
11 ignored, 19 converter room tests, formatting, and strict engine/converter library/test Clippy.

## Sampled boundary crossing

A fresh combined-stack release executable captured 17 successive positions out and back across
Gallows Rock room 369486's west boundary near Creation x=4608. The camera moves from x=4592 to
4624 and returns, facing west. Both modes used Vulkan at 1400×1050 and identical geometry.
All 34 captures settled without pending or failed work.

Outside positions retained every listed mesh, for example 149/149 at x=4592 and 156/156 at x=4607.
Inside positions resumed filtering, for example 49/156 at x=4609 and 55/162 at x=4624. Returning
outside restored 156/156 and then 149/149. The off control retained every listed mesh throughout.
[Exact poses, counts, executable identity, and image hashes](evidence/room-review-20261117/boundary-review.json)
record each step.

This is a sampled sequence of settled camera positions, not a continuous-flight test. The unit
fixture separately covers a 1.19-unit seam between two portal-connected rooms. Neither check
establishes every shipped doorway safe. The [core review record](room-culling-review-20261117.md)
contains the live default-on setting, interior on/off images, and timing protocol.

The later stored-bounds eligibility fix keeps references with incomplete geometric membership
visible before portal-volume testing. This layer passed 71 room-culling-related tests after
propagation. The boundary images above retain their original recorded executable and were not
rerun for the missing-metadata guard.

## Objects outside geometric room membership

A read-only inventory of the five freshly reconverted review cells found 187 of 8,938 references
with neither a containing nor an overlapping `reference_rooms` row. Only 11 had stored bounds,
a model path, no Multibound flag, and a base type eligible for geometric culling. All 11 already
lose their models through existing editor-marker or marker-folder rules: four `BlackPlane01`,
six `XMarker`, and one `NorthMarker`. No bounded renderable candidate remains in this sample.

[Per-cell counts and database identity](evidence/room-review-20261117/unassigned-inventory.json)
and the [read-only SQL query](evidence/room-review-20261117/unassigned-inventory.sql) record the
inventory. For example, run `sqlite3 -readonly -header -csv skyrim_world.db < unassigned-inventory.sql`
against that fixture. The database hash was unchanged after the query.

These counts describe geometric membership, not the authored source-sphere assignment introduced
in #745. They do not count runtime culled meshes or establish coverage across Skyrim. The reviewed
captures therefore do not validate original-game parity for a renderable object outside every
room with no overlap. That case remains unverified; native null-room traversal alone does not
prove object attachment semantics.

## Implicit edges and room overhangs

Implicit connections now pass the incoming visibility volume unchanged. A room box establishes
inferred connectivity but does not bound every child mesh. Clipping to its projected rectangle
could hide an in-frustum child whose reference pivot belongs only to that room.

The seven-unit-gap overhang regression failed before this fix and passes afterward. All 72
culling-related tests on the volume layer pass. The updated overlap fixtures test actual mesh
bounds: an authored doorway still rejects off-aperture meshes and the camera still rejects
geometry behind it. The obsolete room-box projection and rectangle-clipping helpers were removed.

This is a conservative expansion for geometric membership. Authored portal clipping is unchanged;
#745 uses authored joins instead of constructing these inferred edges. The captures and timing
above predate this fix and were not rerun. No new image-parity or performance claim follows from
these tests.
