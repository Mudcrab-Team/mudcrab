# Remaining Skyrim culling parity gaps from REA

This pass compares Mudcrab `9ab2faab` with the installed SkyrimSE.exe 1.7.104.0. REA confirms several rules that were previously described only approximately: Boolean inclusion and exclusion predicates, FIFO room traversal with a shared-node visited gate, and selective loss of inherited constraints at a complexity threshold. None of these findings establishes complete rendering parity.

Independent authored occluders now have export and runtime support, described below. The sections below describe measured traversal, linked plane references, and remaining limits on bounds and occlusion.

The occluder implementation was later ported onto the current #656 dependency head
`e36bba2a`. See [follow-up validation](occluder-review-20261117.md) for the new branch's
checks and visual controls; the native investigation below retains its original baseline.

## Scope and evidence

The executable SHA-256 is `846efccf0c1374d71f892907f46549560f2fcb0a75cb87a3eed438baa0f1402f`. Addresses below are RVAs. Add the preferred image base `0x140000000` for the static analysis addresses used in the saved evidence. A runtime investigation must measure the actual loaded base.

REA 6.9.0 used Ghidra 12.2 with an 8 GiB heap and an owned read-only copy of the existing `SkyrimCulling.gpr` Program. It did not import or auto-analyze the image again. Candidate function names and comments in that Program are prior research annotations, not original symbols or fresh discoveries. The Program's import digest matched the admitted executable.

REA's static Rizin 0.9.1 integration cross-checked machine instructions after the Ghidra session was closed. It used the PE loader, without the raw `base_address` option. No game process was launched, attached, injected, or patched.

The evidence identifiers below refer to locally retained static-analysis responses and snapshots.
Raw analysis artifacts and game binaries are not included in this change.

| Evidence | Main observations |
| --- | --- |
| `ev_4314fd93fb2257fda603dea065965a3d0ad950bdcf6f0a9d570016ffc52acc28` | Traversal, occluder dispatch, and expression threading |
| `ev_e15e8c7b46360d2131d55aa24642eab5e3813200d43f2a29d004b6aced295574` | Sphere evaluator, exclusion builders, path merge helpers, shared volumes, and preparation |
| `ev_d73d1ff449a4be564998cadc3041fc79938a37c3d582ab8137ff52f8ffcd7131` | Group operations, portal boundaries, and box-helper inputs |
| `ev_568fe2b43e3ed60289f3a32f1de1ed2a6d3b5bb5bc2a7991b68e948647b4340a` | Room/shared-node consumers and culling modes |
| `ev_78e6e7ac111c361ca5a26d83588f8351f7ee3ef5402dbf223eeccfd916aa3c8f` | Rizin cross-check of the walk and sphere evaluator |
| `ev_7fdcf5a697268f92df092703cab915a0d69ae55bdb656961c8e6d8846707ab0a` | Small graph accessors, portal RTTI getter, and an unresolved geometry helper |

The saved project has incomplete function-body metadata. `procedure_assembly` returned only the first instruction for the walk despite full pseudocode. Three helper lookups were unresolved. Rizin confirmed relevant bytes independently; empty or one-instruction metadata was not treated as absence of code. An extra disassembly probe starting inside an unvalidated instruction range was not used for conclusions.

## Occlusion requires Boolean expressions and sphere predicates

The evaluator at RVA `0xff7790` reads the scene object's center at `+0xe4` and radius at `+0xf0`. It evaluates a threaded expression over six-plane sets. The group builders and threading routine establish the following operations:

| Encoding | Meaning established by this trace |
| --- | --- |
| 4 | AND group |
| 5 | OR group |
| 6 | End group |
| 7, followed by a plane-set index | Inclusion predicate |
| 8, followed by a plane-set index | Exclusion predicate |
| 2 and 3 | Accept and reject terminal states |

For an ordinary finite sphere with positive radius `r`, each active plane uses `d = dot(normal, center) - plane_constant`.

- An inclusion leaf fails when any active plane has `d <= -r`. It accepts a sphere that intersects the volume even when the sphere is not wholly inside.
- An exclusion leaf fails only when every active plane has `d >= r`. A sphere that crosses an occluder boundary remains eligible. Full containment is required to reject it.
- Inclusion with no active planes accepts. Exclusion with no active planes rejects. These cases cannot share a single "empty volume" interpretation.

The evaluator clears satisfied plane-mask bits as a hierarchy optimization. `BSCullingProcess::Process1`, RVA `0xfeda70`, saves and restores compound active-plane state around traversal. A port can use pure predicates instead of mutable masks, but it must preserve their logical result and avoid sharing narrowed masks across unrelated objects.

The box exclusion builder at `0xff86d0` emits an OR of two exclusion leaves. Its helper at `0xfe6d30` copies two cached plane sets from shape offsets `+0x54` and `+0xc4`. One paired-plane branch in `0xff8130` emits a mixed expression equivalent to `E0 AND E1 AND (E2 OR E3)`, with four exclusion leaves. Exact shape-link construction and the meaning of the linked pointer fields remain unresolved.

These observations rule out implementing all native occlusion as a flat list of world-space boxes. They also rule out subtracting every encountered occluder globally from the union of all portal paths. Incoming constraints and their exclusions belong to their contributing path.

At the start of this investigation, Mudcrab retained only inclusion half-spaces and unions of positive volumes. The implementation below adds path-local exclusions without reproducing the native expression VM. Exact linked-shape algebra remains unresolved.

## Traversal is ordered and does not compute an unrestricted fixed point

RVA `0x15100b0` removes the first queued room, shifting the remaining elements. New destinations are appended. This is FIFO traversal, unlike the LIFO pending vector in the original Mudcrab comparison.

Before considering a portal, it looks up the portal's shared node at `portal + 0x128` in the graph entry's visibility map. A present entry skips that portal. After a portal is processed, instructions at RVAs `0x15106d4` through `0x15106e6` allocate an empty compound frustum and insert it under that shared-node key. This is the visitation mark; the shared-volume builder later initializes that entry from endpoint visibility.

A destination can be queued again and can gain another expression contribution. That does not imply previously processed outgoing portals are reconsidered. The shared-node mark still gates them. Thus an unrestricted union of every path to convergence is not established native behavior. The controlled cycle and overlapping-start renders below show an effect beyond an already-used exit.

Mudcrab processes each containing authored room in graph order. It finishes that start’s FIFO walk before seeding the next room, keeps the shared set of crossed portals, and merges contributions before queuing a destination. An exit carries all paths that reach the source before its first successful crossing. A later path can open a previously missed exit. In the controlled Skyrim 1.7.104.0 cycle fixture, both side markers pass in C, while only the short-path marker passes beyond the shared exit into D.

Ordinary converted packs visit portals in ascending portal FormID order. Ambiguous multi-row portals and legacy links retain conservative traversal. A path carrying occluder exclusions can contribute visibility, but aperture clipping alone cannot establish that it consumed the exit.

## Portal order within one starting room changes the image

In a controlled interior on retail SkyrimSE 1.7.104.0, A contains the camera and opens left into C and right into D. C and D share a wide portal. The two plugins exchange only the placement order of the two entrance records. FormIDs, endpoints, marker positions, camera transform and projection agree across the two combined views. A is the sole containing room in both. Three read-only observations per case verified the main-view root, portal lists and room ownership. A separate camera-bracketed screenshot supplies the marker judgments.

| Live portal order in A | Rooms accumulated | Gems visible in C | Gems visible in D |
| --- | --- | --- | --- |
| Left then right | A, C, D | Left | Left and right |
| Right then left | A, D, C | Left and right | Right |

The first room out of A can contribute through the shared portal. The other room gains that contribution and finds the portal already crossed. D’s own list also changed order across these loads. Its entrance from A had already been crossed when D was processed. The gap gem stayed hidden in both rooms. The direct room views showed every gem. With just the left or the right entrance, both rooms showed only that side. The geometry deliberately puts apertures away from destination boundaries to isolate ordering. A corresponding image difference in a shipped interior remains unmeasured.

The build iterates the cell reference set by physical slot (RVA `0x2c219a`). Portal construction appends each portal to both endpoint lists (RVAs `0x2d7d8e`–`0x2d7db8`). Traversal reads those lists from the head (RVA `0x15100b0`). Filtering the observed portal registrations to each room’s endpoints reproduced every captured list. Projecting archived stock-cell reference slots onto their known endpoints also gives different orders across loads of the same Wolfskull Cave and NchuandZel assets. The stock evidence measures slot order only.

The [regression fixture](../../crates/engine/tests/fixtures/room-portal-order.json) records the two orders and four controls. Without the load-specific order, Mudcrab repeated the left-first result for both loads. For a matched comparison, [prepare_portal_order.py](../../scripts/research/prepare_portal_order.py) writes a private database copy with a complete sequence of portal references for one cell. The normal graph loader uses that sequence on both sides of each portal. Ordinary packs use deterministic FormID order because the asset records alone cannot select a particular runtime allocation.

## The 26-operator fallback retains local constraints

In the fresh-destination branch, native instructions compare the incoming operator count with 25. An incoming count of 26 or more discards the incoming expression for that branch. The destination still receives its new portal inclusion and destination occluder expressions. The branch can also mark the normal view-frustum test as bypassed.

The existing-destination merge branch wraps or extends an OR expression and does not apply that same count check there. The observed threshold is therefore neither a global 26-operation maximum nor a limit on path depth or plane count.

Controlled synthetic interiors on retail SkyrimSE 1.7.104.0 used narrow entrances, a wider exit, and three soul gems beyond it. The left gem lay inside both apertures. The center gem lay inside the exit alone. The right gem lay outside that exit. All three rendered from a direct view. The table pairs the rendered gems with counts read from the main-view entry. Each pose had three agreeing pairs of read-only snapshots. Screenshots were bracketed separately by camera reads.

| Route from the eye | Incoming records | Destination records | Gems rendered |
|---|---:|---:|---|
| Six narrow doors in series, then wide exit | 22 | 26 | Left |
| Eight parallel narrow doors, then wide exit | 24 | 28 | Left |
| Seven narrow doors in series, then wide exit | 26 | 2 | Left, center |
| Same seven doors, local plane in front of center gem | 26 | 6 | Left |
| Twelve parallel narrow doors, exit misses every incoming path | 40 | Room unreached | None |
| Twelve parallel narrow doors, destination already has a separate right-hand route | 40 | 48 | Left, right |
| Twelve parallel narrow doors, exit into unbounded space | 40 | 44 | Left |
| Seventeen parallel narrow doors, wide exit | 60 | 2 | Left, center |
| Seventeen parallel narrow doors, exit misses every incoming path | 60 | Room unreached | None |

The seven-door result isolates the threshold. The six-door destination itself reached 26 records and still retained the narrow view. One door later, a source already at 26 produced a destination with just the new portal predicate and its operand. Predicates with operands and balanced group delimiters give even counts in these fixtures, making 24 the nearest tested count below 26.

Mudcrab now counts the records for each authored room independently of the convex volumes it retains. Skyrim’s OR builder at RVA `0xff8bb0` scans operand words as well as opcodes when deciding whether it can extend an existing group. The plane-set index 4 looks like a group opener in that scan. The eight-entrance and twelve-entrance captures measured 24 and 40 records respectively. Counting apertures or flattened Boolean leaves alone gives the wrong threshold.

The earlier 16-contribution work limit also widened a room before its exit was tested. With seventeen entrances, Mudcrab drew the right gem through an exit that Skyrim never reached. Authored traversal now allows at least one contribution per physical portal in the cell before its independent resource guard can widen the room. The 64-plane guard and conservative handling of larger combinations remain.

The [marker replay](../../crates/engine/tests/fixtures/portal-operator-limit.json) supplies the measured camera, room and aperture geometry, and rendered outcomes for the production traversal test. Run `cargo test -p engine --lib operator_limit_matches_retail_marker_visibility`. Fourteen cases replay the retail visibility decisions using the converted gem bounds and Mudcrab’s normal room-graph loader. The captures used purpose-built cells. Their frequency in shipped interiors remains unknown.

Two captures used the same plane in the destination. One left ownership automatic and one explicitly targeted that room through `XMBR`. In both, Skyrim built six destination records and hid the center gem. Mudcrab accepts a named plane when the converted source confirms a single valid target and the plane lies in that one containing master room. Earlier exports need reconversion for this case. Complex linked occluders, precise admission of occluders by an inherited expression, and the effects of bypassing the ordinary camera frustum remain open.

## Camera and portal boundary rules are more specific than a distance constant

The camera and room-order observations in this section used SkyrimSE.exe 1.7.104.0 (SHA-256 `846efccf0c1374d71f892907f46549560f2fcb0a75cb87a3eed438baa0f1402f`). The main-camera builder at RVA `0x1a97b0` constructs the containment point with this formula. Light-owned preparation at `0x1512540` uses the same calculation.

```
point = camera.translation + camera.near_distance * matrix_column_0
```

The observed fields are translation `+0xa0/+0xa4/+0xa8`, basis elements `+0x7c/+0x88/+0x94`, and near distance `+0x160`.

In retail Treva’s Watch (`00015200`), the eye stayed seven units inside room `00020BAF` beside portal `00020E98`. At headings 345°, 315°, 270°, then 345°, the main-view root was respectively `00020BAF`, `00020BAE`, `00020BAE`, then `00020BAF`. The forward sample was 15 Creation units from the eye. Each heading had three agreeing read-only windows.

Mudcrab uses that distance for the world camera’s room lookup. Portal and occluder geometry still use the eye.

For overlapping starts, Skyrim’s main-camera builder clears the old root at `0xfd96a0` before loading it at `0x1a9859`. On the normal rebuild path it tests each room in live graph array order. Each match invokes traversal at `0x1a9a29`. The first match sets the candidate root. Later matches leave it in place. At `0x1a9a77` the builder stores the selected room in the entry.

In `WolfskullCave01`, the live array placed room `000A9D26` second and `000A9D24` fourth. Entering their 181.5-unit overlap from either room produced the same root, `000A9D26`, at the same measured pose. Both rooms appeared in the accumulated-room list and visibility map. At a lower point in the same overlap, `000A9D24` had the closer center and `000A9D26` still won. Each result used three accepted read-only windows. The opposite approaches ran in separate private launches after a presentation fault. The pair kept its order across launches, while two other rooms exchanged array positions. Mudcrab loads rooms in ascending FormID order, which puts this pair in the opposite order.

A second overlap in `NchuandZel01` put `000A3C1B` before `0005F6C8` and selected `000A3C1B`. A repeat at the same two poses brought the comparison to five cell loads across three processes. Each cell produced two different full room arrays from identical assets. Its observed overlap root stayed the same. Sorting by FormID, first source record, or winning override reproduced zero of the five full arrays. Ascending FormID also chose the other root at both poses.

On the pinned executable, the interior load path reads the cell reference-set storage at cell `+0xb0` and its capacity at `+0x94`. The loop at RVA `0x2c219a` scans 16-byte slots from low to high, skips empty entries, and calls the reference visitor at `0x2d1fe0`. The room-marker path reaches `0x2d77d0`, then calls `0xfda080` at `0x2d7b58`. That helper skips existing room pointers and appends a new room at the old array length. Independently filtering captured occupied slots to the bound room references reproduced the complete graph arrays for five stock loads and both synthetic overlap loads.

The set insertion path at `0x2dc2d0` hashes the eight-byte reference pointer and resolves collisions against other cell references. In the two NchuandZel loads, all 844 placed-reference identities matched. Only 19 kept their runtime address and 15 kept their physical slot. Room `0005F6CA` kept its address and home bucket 792, yet occupied slots 792 and 560. No single order stored at conversion time can reproduce both arrays. For exact replay, bind the ordered room reference FormIDs from the particular retail load to the corresponding converted cell. Neither stock overlap showed its selected root reversing.

A controlled retail render demonstrated a visible consequence of starting order. In one synthetic interior, overlapping rooms A and B open through separate left and right apertures into C, which has a single wide exit into D. Exchanging the two serialized starting-room records preserved every decoded record and FormID but reversed the observed containing-room order. The measured camera and each room’s portal list matched. With A first, both side markers rendered in C and only the left marker rendered in D. With B first, both rendered in C and only the right marker rendered in D. The center gap stayed empty in both rooms. Direct views showed all three markers in each destination, and each single-aperture control rendered its own side through both rooms. This establishes an image difference in the retail executable on the synthetic layout. Its frequency in shipped interiors is unknown.

The [overlap replay fixture](../../crates/engine/tests/fixtures/overlapping-room-starts.json) records both native graph orders, the common camera and apertures, the converted soul-gem mesh bounds, and the six screenshot judgments per order. The engine test rebuilds each graph with the original FormIDs and checks the production mesh visibility decision for each gem. Left-first expects left and right in C and left alone in D. Right-first expects left and right in C and right alone in D. Normal pack loading still uses ascending room FormID; an exact replay supplies the order from its own retail capture.

The portal inclusion builder at `0xff7df0` clears its new plane set's active mask when the absolute portal-plane distance from the compound viewpoint is at most five units. That affects the new inclusion leaf, not every inherited or occluder constraint.

`WithinFrustumDistFirst` at `0xfe94e0` has a different near band. The decompiled scalar combines the portal-plane distance with a supplied view-frustum-plane distance. Its `[-5, 5)` branch also tests the projected point against aperture extents and performs an exact RTTI comparison. The compared RTTI address agrees with the verified `BSPortal::GetRTTI` getter at `0x213c50`. Mudcrab’s `PORTAL_OPEN_DISTANCE` preserves the incoming visibility volume when the eye is within eight Creation units of the doorway rectangle and the aperture survives inherited clipping and any orthographic eye plane.

Exact plane orientation, normalization, corner winding, and boundary behavior still require controlled native fixtures. A read-only boundary experiment can resolve the coordinate convention and cache behavior without debugger attachment; the experiment plan is retained locally.

### Gallows Rock west doorway

In stock `GallowsRock01` (cell 86643), portal 370165 connects room 369486 to unbounded space at Creation x=4607.999. Its live corners span y=−4128 to −3872 and z=0 to 320. The earlier [1.19-unit gap test](room-gap-review-20261117.md) used synthetic room boxes. At the stock doorway the room lookup changes 15 units before the eye crosses the plane when facing west.

On SkyrimSE 1.7.104.0 (executable SHA-256 `846efccf0c1374d71f892907f46549560f2fcb0a75cb87a3eed438baa0f1402f`), the built-in free camera held y=−4000, z=160, horizontal FOV 75°, and a 1400×1050 viewport. Each row below had three agreeing read-only observations of the main camera's root, accumulated rooms, visibility-map room keys, and unbounded-space flag. Unbounded space was visible in every case. IDs are decimal placed-reference IDs.

| Eye x | Heading | Starting room | Reached rooms |
|---:|---:|---|---|
| 4626, then 4623.5 | 270° | 369486 | 369486, 369651 |
| 4622.5 | 270° | Unbounded | 369486, 369651 |
| 4592 | 270° | Unbounded | 369651 |
| 4626 on return | 270° | 369486 | 369486, 369651 |
| 4588 | 90° | Unbounded | 369270, 369271, 369482, 369486 |
| 4592 | 271° | Unbounded | 369651 |
| 4592 | 269° | Unbounded | None |

Room 369651 has an outside portal beyond the screen edge in the west-facing poses. The native portal test at RVA `0xfe94e0` can admit it after the ordinary rectangle test fails. It projects the eye onto the portal plane, compares near-plane distances, and tests the displacement from the aperture center against two rows of the portal rotation. Retaining the aperture's own edge planes lets later portal tests use that path. Camera planes still constrain Mudcrab's mesh tests.

The [captured inputs and decisions](../../crates/engine/tests/fixtures/gallows-rock-seam.json) replay through the production graph loader and traversal. Run `MUDCRAB_SEAM_DATABASE=/path/to/skyrim_world.db cargo test -p engine --lib gallows_rock_seam_replay -- --ignored` with a complete conversion. Run the separate `gallows_rock_seam_model_owners` test with the same command and a reconverted pack to check the two mushrooms and chandelier against their captured room ancestor. The traversal test checks all room IDs, the root, and unbounded space using both the captured and default render near distances. The read windows are non-atomic. A room-map entry alone cannot identify an individual submitted mesh.

At the outside west-facing pose, Mudcrab retained 27 of 149 main-camera meshes. With the same geometry and the whole-cell fallback forced in a private copy of the database, it retained all 149 and exposed a distant shell that was absent in the Skyrim capture. The fixed view removed that shell. Looking back into the doorway preserved the interior in both engines.

A second capture isolated the remaining west-edge geometry at x=4592, heading 270°. The detached patch came from mushrooms `00042177` and `00042178`. The thin chain and fixture overlapping the arch came from chandelier `0005A22A`. In the retail main view all three had room `000AA079` as an ancestor. That room had no visibility-map entry at the outside pose. A camera placed inside the room confirmed the same ancestry with a mapped room and visible mushrooms. Mudcrab had no source sphere for any of the three models and kept them.

On the pinned binary, `NiSwitchNode::UpdateWorldBound` (RVA `0xeee080`) copies the active child's world sphere when switch flag bit zero is set. Otherwise it uses the ordinary child-bound merge. Both mushroom switches selected child zero. Empty `BSValueNode` attachment points on the chandelier contributed no radius. The puddle model uses a `BSMultiBoundOBB`; its sphere calculation (RVA `0xfcf150`) uses the box center and the length of its three extents. Puddles added mesh-list entries but produced no colored pixels in the isolated outside capture.

After reconversion, all nine probed placed-reference spheres agreed with the retail center and radius to within 0.001 Creation units. The outside camera retained only the five meshes of the stair arch (previously 27 of 149). Its image matched a render of that arch alone. Adding just the five missing base-model sphere rows to the old pack reproduced the reconverted images at all eight original seam poses. Both mushroom references were retained in the inside-room control. Background, lighting, and glow still differ between renderers. The observations cover the initial unharvested state and the main view. [Sphere measurements, mesh counts and image hashes](evidence/room-review-20261117/gallows-rock-reconversion.json)

## Separate graph submission and culling modes remain relevant

Static accessors at `0xfda6c0` and `0xfda6e0` read the graph array at `+0x58` and its count at `+0x68`. The pinned community declaration names that array `alwaysRenderChildren`. Preparation submits its entries after the portal work, separately from the graph's unbounded-space list. This establishes submission independent of room reachability, not unconditional drawing outside the ordinary view frustum.

Room `OnVisible` at `0xfcddd0` gates normal traversal on its mapped volume, temporarily installs the room volume, applies its culling mode, visits children, and restores surrounding state. Modes 1 and 3 have special bypass handling. The shared-node consumer at `0xfe9e20` also uses graph-entry and mode state. Mudcrab's filter currently edits mesh visibility for the main or opted-in camera, while lights and shadow caster lists remain independent.

The two preparation paths are owned by a shadow scene node, but that class name does not identify which render pass each call serves. Full light/shadow pass ownership, the producers of always-render membership, and room lighting/image-space application remain unresolved.

## Current priorities

| Priority | Work | Status after this pass |
| --- | --- | --- |
| 1 | Export authored occluders and add path-local inclusion/exclusion expressions with sphere-aware tests | Implemented for independent static planes, boxes, and isolated same-owner plane pairs. Complex links, stateful shapes, and unverified overrides remain inactive |
| 2 | Match FIFO traversal, shared-node visitation, path merges, and selective operator fallback | Sequential starts, FIFO traversal, and shared portal visitation implemented for unambiguous authored graphs. The 26-record fallback matches the fourteen controlled main-camera cases. Both measured portal orders replay from captured registration sequences. Stock-cell rendered impact and more complex occluder combinations remain open |
| 3 | Reproduce authored room order and portal boundary predicates | Main-view selection follows live graph array order. Pack order is deterministic, while individual retail loads can differ. Exact portal boundary predicates remain open. |
| 4 | Finish actor, forced-assignment, primitive-override, and moving-child paths | Existing static/source-sphere ownership is partial; exact update and ordering behavior remains open |
| 5 | Match current native bounds and deformation | Mudcrab's final mesh AABB test differs from native sphere tests; ragdoll and animated bounds remain separate rendering work |
| 6 | Establish light/shadow pass ownership and always-render/room-metadata semantics | Submission and mode evidence improved; complete pass parity remains unresolved |

Already implemented work remains in place: positive visibility volumes, mesh-bound filtering, authored unbounded endpoints, portal-shared endpoint unions, joined room boxes, source-sphere membership, initial named overrides, `NONE`, and supported root-transform reassignment.

## Independent occluder implementation

The converter exports winning interior `PlaneMarker` (`0x17`) references into `room_occluders`, preserving shape, placement, scale, header flags, enable-parent presence, explicit multibound target, and raw ordered `XORD` links. Shape 1 is a box; shape 3 uses the primitive's X/Z rectangle. Occluder-only interiors receive graph metadata too. A trusted full conversion publishes `occlusion_version=1`; older databases retain their previous behavior until reconverted.

Static REA decompilation of the same executable confirms that the record loader at RVA `0x15b040` reads the 36-byte `XOCP` payload into unused scratch storage. Its `XPRM` branch instead calls primitive creation at `0x212710` and stores the result through `0x16d700`. `DATA` and `XPRM` therefore supply the exported geometry; conflicting `XOCP` values do not override them. Evidence: `ev_9c71e2a1374129c1dc85b77f7d3130f7ca820e607b3974115b6bc3d1695e3bbf`; the construction response is retained locally. REA 6.9.0, Ghidra 12.2, and the executable identity above were used on an owned project copy; no game-process attachment occurred.

Native assignment at RVA `0x2c9b90` substitutes an occlusion shape's translation for its rendered bound center, then associates it with a containing room before portal-shared classification. Mudcrab applies that rule after joined-master resolution. No containing room assigns the shape to unbounded space. Several distinct containing masters leave the occluder inactive because native hash iteration order is unresolved.

The runtime builds perspective shadows or orthographic prisms from independent planes and boxes. A box shadow intersects silhouette half-spaces with every camera-facing cap, preventing premature rejection before the ray reaches the box. Each contributing room path carries its own exclusions through later portals. Separate paths remain a union; budget widening retains local exclusions while conservatively dropping inherited constraints.

Exclusion testing uses a sphere enclosing every corner of the mesh's affine-transformed AABB. Full sphere containment is required; partial overlap, touching boundaries, invalid bounds, near-camera shapes, and shapes crossing the near plane retain geometry. This sphere can exceed Skyrim's current node sphere, so it can retain extra meshes. Geometric and ECS tests cover these rules. Retail comparisons below cover the listed main-camera cases.

Disabled/deleted references, `NONE`, named overrides outside the verified same-room plane case, enable-parent-controlled shapes, and linked components outside the isolated same-owner plane-pair case remain inactive. Recoverable links survive invalid source geometry so a linked neighbor cannot accidentally become independent. Malformed link framing marks the source cell incomplete. Lights, shadow lists, predicting load-specific portal order, linked-shape Boolean construction, activation updates, and native screen-space/complexity heuristics remain separate work.

## Linked plane references in retail

SkyrimSE 1.7.104.0 under Proton rendered two independent shadows for an isolated pair of static
`PlaneMarker` references connected by `XORD`. The 17 camera-bracketed captures used two authored
ESPs, one room per case, and three soul gems at fixed positions. The executable SHA-256 was
`846efccf0c1374d71f892907f46549560f2fcb0a75cb87a3eed438baa0f1402f`. A scan of placed
references in the five installed base-game and DLC masters found no `XORD` fields.

| View and authored planes | Retail gems visible (left, seam, right) | Cases |
|---|---|---:|
| No planes | all three | 1 |
| Left alone / right alone | seam and right / left and seam | 2 |
| Two coplanar planes, unlinked or reciprocal links in eight slot arrangements | seam only | 9 |
| Folded planes, unlinked, reciprocal, one-way, or temporary reciprocal | seam only | 4 |
| Camera past both folded planes, reciprocal links | all three | 1 |

Three matching pairs of read-only memory snapshots per case identified the world cullers’ room
entry. With both planes in front of the eye, the room program contained two exclusion leaves
with distinct plane sets. All four partner pointers on each render shape were zero. In the
folded linked cases the reference handles resolved to the intended opposite reference. In the
one-way case only the source carried the handle. The camera-past control contained no exclusion
records. The snapshots and images were taken separately at stationary poses.

Mudcrab previously skipped both endpoints of every linked pair. The coplanar reciprocal case
therefore kept both gems that Skyrim hid. After reconversion, an isolated, source-verified pair
of static planes with the same owner uses the ordinary plane exclusion for each endpoint.
The [regression fixture](../../crates/engine/tests/fixtures/linked-occluders.json) records the
placements, measured camera frusta and retail visibility used for mesh-bound replay. Running
the typed converter on the two captured ESPs with a minimal master reproduced the fixture’s
30 plane rows and all 23 verified link rows.

Larger link networks, cross-owner pairs, state changes, and render shapes whose partner pointers
are populated remain unverified. These captures do not exercise the four-leaf native builder.
