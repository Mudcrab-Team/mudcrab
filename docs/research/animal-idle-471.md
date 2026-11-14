# Authored creature idle support (#471)

Source: `64108a1c38b0a854d4f82d7031ea50b57c336a5a` plus the patch described below.
Owner: Taylor; prepared with Codex, 2026-10-31. Later sections record the review state through 2026-11-01.

## Contract and observed failure

Taylor identified the cow beside the Riverwood spawn and chickens, then reported
all observed animals static. The playtest used grid `(5, -12)`, worldspace
`0000003C`, and the immutable existing conversion:

`user-testing-main-b5259d33` (`<registered pack path>`, resolved with `workspace.py pack-path`)

Location candidates, not confirmed reference identities:

- Cow `00060B22` in Riverwood04: base `00023A90` (`EncCow`), race `0004E785`.
- Chickens `0010962A`, `001066DF`, `001066DE`: base `000A91A0` (`EncChicken`),
  race `000A919D`, in Riverwood04, Riverwood, and Riverwood02 respectively.

Their standalone converted bodies contain no animations. The original pack kept
character-only HKX inputs and six human gait outputs. Streaming installed clips
only for composed human bodies. Human idle target paths do not match either animal.

An initial cow/chicken patch followed each species' own project, skeleton and
`mt_idle.hkx`, validated every target against the existing body, and supplied a
private read-through overlay. Taylor explicitly confirmed **“It worked.”** Native
receipt: `issue471-playtest-20261031T120249Z` under shared `evidence/`; executable
SHA-256 `8918d6bde4ee982837d9034afce4e17f20f03fc9d5cc262f8074614cd4676de2`.
Game exit was 0 and baseline hashes were unchanged. This confirms those two
species only. The subsequent generic implementation requires separate evidence.

## Generic implementation

The supported contract is standalone transform-pose playback for authored creature
clips over their own compatible converted body. Discovery follows winning RACE
ANAM/MNAM/FNAM skeleton and NAM3/MODL project links, then project → character →
HKX rig and declared animation sources. Full paths identify assets; species names,
human bone names, and a shared `NPC Root [Root]` string never establish compatibility.

- All declared creature clips are candidates, including arbitrary filenames.
  Conservative behavior metadata traversal supplies idle diagnostics, not graph execution.
- Explicit project-relative standing-idle choices cover the 46 shipped nonhuman
  RACE projects. They must be declared by that character. Unknown projects require
  one unambiguous discovered idle; ambiguous or missing defaults remain unresolved.
- Bone paths and reference transforms come from the actual exported body GLB.
  Exact or case-unique names bind; skipped tracks are recorded. A moving clip
  with no moving body target is not applicable. Authored static poses stay static.
- `meshes/actor-idles/catalog.json` records race, sex, full body, RACE skeleton,
  project, default, all published sources, exclusions and failures. Optional SQL
  `race_animation_sources` lets runtime reject stale authoring identities.
- `authored-actor-idle-v2` authenticates body, rig, source and complete bone-tree
  context. Identical trees may share clips across geometry/texture variants;
  catalog context authenticates every body. Parent names, transforms or rig/source
  hashes changing produce different identities. Publication preflights destinations.
- Whole creature skins authored in biped slot 30 become bodies only for confirmed
  nonhuman, non-FaceGen projects, with one compatible candidate and no normal body.
  A normal body wins; ambiguity stays explicit. Human and child composition remain
  on their existing paths.
- Archive recipe v4 keeps mesh HKX inputs, including projects outside `actors/`.
  Current-reader v2/v3 ingestion packs are not reused, so the first conversion after
  this change extracts converter inputs again. Existing pruning rules keep those
  current-reader packs on disk; only pre-reader-version packs retire.
- Runtime loops the verified default and accepts `AuthoredIdleRequest` for a
  source-specific loop or one-shot. Creature actors receive no `ActorDrive`.
  Existing human gait/drive selection and the old cow/chicken overlay fallback remain.
  The fallback also requires its published clip. Nothing outside tests sends
  `AuthoredIdleRequest` yet, so only the default loop plays in game.
- A creature reference whose `ACHR` header has `Starts Dead` (`0x200`) receives no
  idle. The engine has no death pose or ragdoll, so the corpse keeps its bind pose
  instead of standing in an idle loop. Human corpses keep their existing behavior.
- Contracts are unique by race, sex and canonical body path. Authored part paths
  differing only in case or separators share one binding. A body path that cannot
  name a mesh, or a binding/clip key the catalog would reject, becomes an unresolved
  body or clip issue. With `fail_fast`, these remain conversion errors, like other
  unresolved bodies.

## Coverage and limits

Original source: `<Skyrim SE Data folder>`. Independent source audits found
93 active BSAs, 7,705 HKX paths, no relevant cross-archive path collisions and no
loose `Data/meshes` tree. The retained extracted corpus is therefore suitable for
this source check; production still uses winning virtual-file lookup.

The decoder audit covered 46 projects and 2,733 declared clip/rig contexts:
2,626 decoded, 107 failed. Failures included paired rigs, nonfinite spline values
and out-of-range skeleton indices. All 823 conservatively identified idle contexts
(765 distinct sources) decoded; two sources were additive spell-release layers.
Those layers require an explicit base pose and are not standalone idles.

The retail Rust probe confirmed 259 additional NPC bases and 40 additional
race/sex/body contracts from the slot-30 fix. Of 222 body contracts, 221 published
defaults across all 46 projects and 126 races; none remained unresolved. One
detect-life helper scene (`meshes/dlc02/clutter/dlc02detectlifebox.glb`) had no
bound transform tracks and was explicitly excluded.

The supplement contains 3,248 deduplicated clips, 2,355 distinct source keys and
18,617 body/clip bindings. All 821 standalone idle contexts published: 763 distinct
idle sources, 979 outputs and 4,761 body/idle bindings. The two additive layers
above are the only discovered idle contexts absent from the standalone library.
The wider all-declared inventory retains 724 paired/additive exclusions and 61
decode failures over 12 non-idle source keys. Those failures are not silently
counted as supported clips.

Headless playback passed every published binding: 18,617 across 221 bodies,
including all 4,761 standalone idle bindings. It checked motion for all 2,555
distinct moving rig/source contracts, counted 432 authored static bindings, and
verified one-shot completion. The independent coverage audit matched every
playback record to its catalog entry and checked all inventoried idle contexts.

This patch does not execute behavior transitions, variables, blend masks/modifiers,
paired actors, additive layers, root-motion policy or float-track effects. Selecting
a source clip does not reproduce its graph timing, events, speed or blend context.
It does not repair unsupported codecs or infer missing exported geometry. A scene
without compatible targets is reported separately; that is not proof its authored
source was intentionally static. Modded projects and changed conversion inputs
need their own contract validation. Generic CPU success does not prove native
visual parity or that every creature has been observed in game.

## Coverage denominator

The counts above describe bodies that reached an actor model and a non-character
RACE project. The review overlay database gives the placed-reference scope. It has
14,093 placed `NPC_` references:

- 3,163 reference a catalog binding. 1,020 of these have `Starts Dead` and now
  receive no idle player. This does not supply a death pose or ragdoll.
  Draugr-project corpses are the largest group.
- 8,206 have no resolved actor model, and the engine skips them ("actor spawn
  skipped"). 8,192 have an unresolved Traits chain, chiefly `Lvl*` bases whose
  template is a leveled list. 14 are other unresolved bases, such as
  `dunLabyrinthianUndeadDragon`. These animals are absent, not static. This
  predates #471 and needs leveled-template resolution, which this patch does not add.
- Bodies under the shared character project stay on the human and child paths.
  Child bodies (44 references), Astrid's body (1) and mannequins (154) get no
  idle. The conversion notice counts these bodies separately.

Two published defaults are combat idles: dragon `ground_combatidle.hkx` and
ballista centurion `combat_idle.hkx`. Whether they read as resting poses needs a
visual check.

## Evidence and review

Shared storage root is obtained through `workspace.py paths`, respecting
`MUDCRAB_WORKSPACE_ROOT`. Reports are task-owned under `evidence/`; retail bytes
and private supplements remain outside Git. No registered pack was mutated.
The receipts named below are private to the author's machine. They are not in this
repository, so a reader cannot check them from the repository alone.

- `20261031-issue471-animal-idle-t3-655029eb`: original two-species source,
  conversion and focused test receipts.
- `generic-idles-20261031T125030Z`: declared-source decoder audit.
- `issue471-full-idles-20261031T154240Z`: preserved failed archive fixtures
  following the v4 retention change; subsequent checks use a new receipt.
- `issue471-full-idles-20261031T155248Z`: strict Clippy; 1,045 converter tests,
  51 contract integrations, 56 shared tests, 54 actor tests and 290 streaming
  tests passed. The retail export passed and preserved baseline hashes. Its first
  playback run exposed a test-harness read after startup consumed `ActorClips`;
  the failure is retained. The correction inspects the installed player graph.
- `issue471-playback-20261031T160000Z`: preserved harness timing failure on
  storm atronach `rootf.hkx`. Its pose curves equal `rootb.hkx`; Bevy threads a
  new graph one frame after installation. The test now evaluates time zero
  before recording its baseline, avoiding a comparison against the prior clip.
- `issue471-playback-20261031T160509Z`: final format, strict engine Clippy,
  full-library headless playback and independent coverage audit passed.
  Its `reuse-proof.json` applies only to that historical snapshot.
- `issue471-repairs-20261101T050109Z/recovery-20261101`: the post-review repair
  campaign passed 1,686 converter tests in 106 suites, 56 shared tests, 1,240
  engine tests and one reference-loader test. Retail export and all 18,617
  playback bindings passed. The probe example was added later and has a separate
  compile receipt; those test totals do not cover the probe.
- `issue471-sonnet-recheck-20261101`: two Sonnet 5.5 rechecks found no blocker in
  the four earlier repairs. They identified a modded body-key mismatch, protocol
  evidence gaps and documentation drift. The subsequent source repair shares
  body-path normalization between publication and lookup, verifies CPU sample
  acknowledgements, preserves RACE parse diagnostics and reports uncalibrated
  native motion margins. When this note was written, its focused
  regressions and successor binary had not been run. Earlier passing receipts do not
  certify these changed inputs. This note records no later receipt for that repair.
Later pull request checks and test runs for this stack are not recorded in this
note. Check the current head's CI result for the present state.

The generic retail probe builds a reduced actor fixture and an animation-only
supplement using immutable baseline geometry. Its database is **not a playable
world database**. The headless check uses Bevy animation without a renderer,
window or GPU, and verifies target binding, source-derived motion intervals and
one-shot completion; authored static bindings are counted separately. Native
review remains a separate gate. The final receipt retains source diffs/archive,
input hashes, commands, logs and selected test executables with checksums.

Historical private native-review assets were kept in a task-owned scratch
directory (`issue471-review-20261031T160000Z`) under shared storage. They are not
in this repository. The full
world database was APFS-cloned; only actor tables and race animation projection
were replaced from the validated fixture. Geometry and clips are read through
without copying or changing the registered baseline. `overlay.json` records
hashes and table counts. This overlay predates the repairs: its catalog matches,
but its database and coverage inputs differ from the post-repair export. It is
not admitted for a current native run without new compatibility proof or an
authorized successor overlay. No native launch of that overlay is recorded in
this note.

The actor-only probe bypasses production `spawn_cell`, installs a live clip and
hides unskinned geometry. Its scope is selected skinned-body motion and capture
readback. It cannot prove Starts Dead suppression, production spawn mapping,
corpses, rigid geometry, materials or complete creature acceptance. Starts Dead
remains covered by loader/spawn CPU regressions, not this native proposal.
Native thresholds remain uncalibrated; detecting a moving sample or changed
pixel does not establish adequate deformation. Only cow/chicken have Taylor's
earlier native confirmation. Native memory recovery remains a separate gate.

Modded body/rig pairs also need independent compatibility evidence. Nonzero bound
and moving track counts can include only shared root bones. These counts alone
do not establish full skeleton compatibility. Retail bindings have separate
playback evidence; this does not qualify arbitrary modded skeletons.

Non-default requests currently have no production producer. Requests sent before
an actor library exists can be missed, failed or partial clips retain previous
bone transforms, and a finished one-shot holds its last pose. These transitions
remain outside default-loop acceptance. The A1/A2 CPU helpers read no assets;
their proposed results qualify process protocol and cleanup only.

Issue [471](https://github.com/Mudcrab-Team/mudcrab/issues/471), discussion and
merged [357](https://github.com/Mudcrab-Team/mudcrab/pull/357),
[401](https://github.com/Mudcrab-Team/mudcrab/pull/401),
[402](https://github.com/Mudcrab-Team/mudcrab/pull/402),
[404](https://github.com/Mudcrab-Team/mudcrab/pull/404), and superseded
[381](https://github.com/Mudcrab-Team/mudcrab/pull/381)/
[382](https://github.com/Mudcrab-Team/mudcrab/pull/382) were read before implementation.
The original searches found no matching active animal-idle change. Child NPC
issue #470 and behavior-graph implementation remain separate.
