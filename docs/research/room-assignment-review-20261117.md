# Authored-assignment retail comparison, 2026-11-17

Fresh original-game captures support the two background-shell removals questioned in review.
Skyrim omits those shells at the matched viewpoints too. This supplies the missing retail oracle for these removals. Authored assignment remains enabled
by default. The comparison does not establish full Skyrim parity.

## Retail oracle for the disputed images

The original game is SkyrimSE.exe 1.7.104.0 under Proton Experimental 11.0-100. Its SHA-256 is
`846efccf0c1374d71f892907f46549560f2fcb0a75cb87a3eed438baa0f1402f`.
Built-in `tfc 1` supplied the free camera. Read-only native probes checked NiCamera RTTI, position,
rotation, field of view, cell, and enabled multibounds before and after every capture.

The 15 retail captures cover five poses three times at 1400×1050. Position errors are below 0.02
Creation units and angular errors below 0.002 degrees. A private Proton prefix disabled autosaves.
The installed executable, preferences, and saves matched all nine pre-run hashes after normal exit.
No debugger attachment or instruction patch was used.
[Executable and camera receipts](evidence/room-review-20261117/retail-captures.json) retain the evidence.

![Original-game captures beside the exact historical authored and full-render controls](evidence/room-review-20261117/authored-retail-oracle.png)

The middle and right columns reuse the historical authored runtime from
[the original validation record](room-authored-assignment-20261115.json), rather than relabeling a
newer combined build as that older layer. Their binary and image hashes are in
[the review receipt](evidence/room-review-20261117/authored-review.json).

At Bleak Falls `room1-c`, the large Nordic room shells in the full-render control are absent in
both retail and authored rendering. At Gallows `inside-out`, retail retains the foreground ceiling
edges and the small right-hand opening while omitting the large shell behind it. Authored rendering
makes the same broad visibility choice. Fog, sky, lighting, some props, and material sampling differ.
This is a geometry-visibility comparison, not an RGB parity measurement.

## Named targets and NONE in Katariah

Katariah01 contains 32 named targets, three NONE overrides, 2,536 automatic rules, and 18 unknown
rules. All five selected cells exported complete. The private fixture rebuilt 1,206 source models.
Unknown rules remain conservative.

![Original-game and current combined-stack Katariah floor captures](evidence/room-review-20261117/katariah-overrides.png)

The NONE-floor pose is above reference 769925. Its floor remains visible in all three modes while
the enabled mesh list falls from 153 to 63. The named-floor pose is above reference 457767, whose
authored target is room 454223 while the camera lies in room 454224. Its floor remains visible in
retail and the enabled view, with 48 of 255 meshes retained. The full-render control instead covers
much of that view with a close shell surface. The side-by-side captures show differences in props and material sampling
between Skyrim and both Mudcrab modes. These images do not identify the owner of every visible pixel.

The fresh combined-stack recheck has 48 settled images at eight poses, three runs per mode.
It includes the review toggle and gap fixes plus the upper layer's bounds support. It is recorded
as a combined check, not a standalone rerun of #745. [Inputs, counts, comparisons, and hashes](evidence/room-review-20261117/authored-review.json)
retain that distinction. Local plane/box occluders are absent from both historical and current
executables used here. The broader traversal, lighting, shadows, and moving-deformation limits
remain in the existing research notes.

## Legacy bounds eligibility follow-up

The stored-model-bounds guard applies only to legacy geometric membership. Authored initial
named targets still supply ownership without a model sphere or legacy aggregate bounds. Authored
source spheres still drive ownership and movement updates without that legacy metadata. ECS
controls cover both cases alongside the spanning-floor regression for legacy graphs. This layer
passed all 82 room-culling-related tests after propagation. The retail images above retain their
recorded executables and were not rerun for this missing-metadata guard.

## Assignment cache invalidation

Cached ownership now recomputes whenever the computed cell-local affine transform changes.
A fixed tolerance on matrix entries cannot bound sphere displacement: scaling a source sphere
centered at x=9998 by 1.0005 moves it about five units. The regression keeps one scheduled
system and its local cache across two frames, then verifies reassignment into the visible room.
It failed before the fix and passes afterward; all 84 culling-related tests on the assignment
layer pass. The existing retail captures predate this change and were not rerun.

## Private fixture path validation

The preparation utility checks the supplied asset directory and every ancestor, using an absolute
path, before writing the preparation receipt or opening SQLite for writes. Database hard-link
and direct child-link checks remain in place. The linked-parent regression failed before the fix;
all five utility tests now pass. An actual CLI run with disposable files rejected the linked
ancestor before any write, preserving both the database bytes and an existing receipt. No game
assets were used in that check.

## Subset model replacement

A subset export that changes or removes an existing base model now removes that base's cached
source sphere and marks every referencing cell incomplete in the same transaction. A full room
export rebuilds completeness. Unchanged models retain their spheres and cell metadata; unrelated
bases and cells are untouched. A light that loses its model also loses its old drawable row.

The nonempty-subset regression failed when replacing `A.nif` with `B.nif`: the old sphere survived.
It now covers replacement, removal, and unchanged-model updates for both `STAT` and modeled `LIGH`
records, including affected cells and unrelated controls. This regression checks the derived ownership metadata.
The existing screenshots predate the subset fix.
