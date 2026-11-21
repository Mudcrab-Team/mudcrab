# Remaining Skyrim culling parity gaps from REA

This pass compares Mudcrab `9ab2faab` with the installed SkyrimSE.exe 1.7.104.0. REA confirms several rules that were previously described only approximately: Boolean inclusion and exclusion predicates, FIFO room traversal with a shared-node visited gate, and selective loss of inherited constraints at a complexity threshold. None of these findings establishes complete rendering parity.

Independent authored occluders now have export and runtime support, described below. Traversal order, linked occluders, and exact native bounds remain parity work.

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

RVA `0x15100b0` removes the first queued room, shifting the remaining elements. New destinations are appended. This is FIFO traversal, unlike Mudcrab's LIFO pending vector.

Before considering a portal, it looks up the portal's shared node at `portal + 0x128` in the graph entry's visibility map. A present entry skips that portal. After a portal is processed, instructions at RVAs `0x15106d4` through `0x15106e6` allocate an empty compound frustum and insert it under that shared-node key. This is the visitation mark; the shared-volume builder later initializes that entry from endpoint visibility.

A destination can be queued again and can gain another expression contribution. That does not imply previously processed outgoing portals are reconsidered. The shared-node mark still gates them. Thus an unrestricted union of every path to convergence is not established native behavior. Actual rendered consequences and sensitivity to authored portal order remain runtime questions.

Mudcrab [propagates contributions](../../crates/engine/src/room_culling.rs) through adjacent portals and suppresses geometric duplicates or contained volumes. It has no equivalent shared-node visited gate. Matching native ordering and contribution rules requires a deliberate traversal change, not just another geometric containment test.

## The 26-operator fallback retains local constraints

In the fresh-destination branch, native instructions compare the incoming operator count with 25. An incoming count of 26 or more discards the incoming expression for that branch. The destination still receives its new portal inclusion and destination occluder expressions. The branch can also mark the normal view-frustum test as bypassed.

The existing-destination merge branch wraps or extends an OR expression and does not apply that same count check there. The observed threshold is therefore neither a global 26-operation maximum nor a limit on path depth or plane count.

Mudcrab currently widens a space to the camera volume at 16 contributions or more than 64 planes. This is a conservative policy with different scope. A parity implementation needs to distinguish local portal and occluder constraints from inherited constraints before copying the native fallback.

## Camera and portal boundary rules are more specific than a distance constant

Preparation at `0x1512540` constructs the containment point as:

```
point = camera.translation + camera.near_distance * matrix_column_0
```

The observed fields are translation `+0xa0/+0xa4/+0xa8`, basis elements `+0x7c/+0x88/+0x94`, and near distance `+0x160`. It tests and prefers a compatible cached root, then tests other rooms and starts walks from all containing rooms. Mudcrab uses the camera translation directly and has no equivalent cached root.

The portal inclusion builder at `0xff7df0` clears its new plane set's active mask when the absolute portal-plane distance from the compound viewpoint is at most five units. That affects the new inclusion leaf, not every inherited or occluder constraint.

`WithinFrustumDistFirst` at `0xfe94e0` has a different near band. The decompiled scalar combines the portal-plane distance with a supplied view-frustum-plane distance. Its `[-5, 5)` branch also tests the projected point against aperture extents and performs an exact RTTI comparison. The compared RTTI address agrees with the verified `BSPortal::GetRTTI` getter at `0x213c50`. This is not a generic eight-to-five-unit replacement.

Exact plane orientation, normalization, corner winding, and boundary behavior still require controlled native fixtures. A read-only boundary experiment can resolve the coordinate convention and cache behavior without debugger attachment; the experiment plan is retained locally.

## Separate graph submission and culling modes remain relevant

Static accessors at `0xfda6c0` and `0xfda6e0` read the graph array at `+0x58` and its count at `+0x68`. The pinned community declaration names that array `alwaysRenderChildren`. Preparation submits its entries after the portal work, separately from the graph's unbounded-space list. This establishes submission independent of room reachability, not unconditional drawing outside the ordinary view frustum.

Room `OnVisible` at `0xfcddd0` gates normal traversal on its mapped volume, temporarily installs the room volume, applies its culling mode, visits children, and restores surrounding state. Modes 1 and 3 have special bypass handling. The shared-node consumer at `0xfe9e20` also uses graph-entry and mode state. Mudcrab's filter currently edits mesh visibility for the main or opted-in camera, while lights and shadow caster lists remain independent.

The two preparation paths are owned by a shadow scene node, but that class name does not identify which render pass each call serves. Full light/shadow pass ownership, the producers of always-render membership, and room lighting/image-space application remain unresolved.

## Current priorities

| Priority | Work | Status after this pass |
| --- | --- | --- |
| 1 | Export authored occluders and add path-local inclusion/exclusion expressions with sphere-aware tests | Implemented for independent static planes and boxes; linked, stateful, and overridden primitives remain inactive |
| 2 | Match FIFO traversal, shared-node visitation, path merges, and selective operator fallback | Native structure established; current traversal differs; runtime order cases still needed |
| 3 | Match camera seed point, cached root, and portal boundary predicates | Native formulas narrowed; coordinate and boundary fixtures still needed |
| 4 | Finish actor, forced-assignment, primitive-override, and moving-child paths | Existing static/source-sphere ownership is partial; exact update and ordering behavior remains open |
| 5 | Match current native bounds and deformation | Mudcrab's final mesh AABB test differs from native sphere tests; ragdoll and animated bounds remain separate rendering work |
| 6 | Establish light/shadow pass ownership and always-render/room-metadata semantics | Submission and mode evidence improved; complete pass parity remains unresolved |

Already implemented work remains in place: positive visibility volumes, mesh-bound filtering, authored unbounded endpoints, portal-shared endpoint unions, joined room boxes, source-sphere membership, initial named overrides, `NONE`, and supported root-transform reassignment.

## Independent occluder implementation

The converter exports winning interior `PlaneMarker` (`0x17`) references into `room_occluders`, preserving shape, placement, scale, header flags, enable-parent presence, explicit multibound target, and raw ordered `XORD` links. Shape 1 is a box; shape 3 uses the primitive's X/Z rectangle. Occluder-only interiors receive graph metadata too. A trusted full conversion publishes `occlusion_version=1`; older databases retain their previous behavior until reconverted.

Static REA decompilation of the same executable confirms that the record loader at RVA `0x15b040` reads the 36-byte `XOCP` payload into unused scratch storage. Its `XPRM` branch instead calls primitive creation at `0x212710` and stores the result through `0x16d700`. `DATA` and `XPRM` therefore supply the exported geometry; conflicting `XOCP` values do not override them. Evidence: `ev_9c71e2a1374129c1dc85b77f7d3130f7ca820e607b3974115b6bc3d1695e3bbf`; the construction response is retained locally. REA 6.9.0, Ghidra 12.2, and the executable identity above were used on an owned project copy; no game-process attachment occurred.

Native assignment at RVA `0x2c9b90` substitutes an occlusion shape's translation for its rendered bound center, then associates it with a containing room before portal-shared classification. Mudcrab applies that rule after joined-master resolution. No containing room assigns the shape to unbounded space. Several distinct containing masters leave the occluder inactive because native hash iteration order is unresolved.

The runtime builds perspective shadows or orthographic prisms from independent planes and boxes. A box shadow intersects silhouette half-spaces with every camera-facing cap, preventing premature rejection before the ray reaches the box. Each contributing room path carries its own exclusions through later portals. Separate paths remain a union; budget widening retains local exclusions while conservatively dropping inherited constraints.

Exclusion testing uses a sphere enclosing every corner of the mesh's affine-transformed AABB. Full sphere containment is required; partial overlap, touching boundaries, invalid bounds, near-camera shapes, and shapes crossing the near plane retain geometry. This sphere can exceed Skyrim's current node sphere, so it can retain extra meshes. Independent geometric and ECS tests verify the supported behavior, but no original-game visual comparison establishes exact occluder parity.

Disabled/deleted references, `NONE`, named primitive overrides, enable-parent-controlled shapes, linked planes and their known incoming targets remain inactive. Recoverable links survive invalid source geometry so a linked neighbor cannot accidentally become independent. Malformed link framing marks the source cell incomplete. Lights, shadow lists, native portal visitation order, linked-shape Boolean construction, activation updates, and native screen-space/complexity heuristics remain separate work.
