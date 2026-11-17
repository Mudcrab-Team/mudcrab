# Room-boundary review validation, 2026-11-17

A camera outside every authored room keeps full rendering. Room boxes do not prove that their
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
