# Web and foliage culling mismatches

The original-game comparison exposed two confirmed causes of overdraw: missing source bounds
for foliage and effect-node hierarchies, and blanket activator exemptions. The fixes remove the
distant webs and ferns while retaining them in matched close-up controls.

The [machine-readable report](room-culling-mismatches-20261115.json) records native bounds,
reference identities, source fingerprints, repeated render receipts, and checks. The local evidence
directory is `/home/aivv/.t3/artifacts/skyrim-culling-mismatches-20261115`. It contains native
reference probes, screenshots, capture logs, the contact sheet, and the comparison viewer.
No proprietary assets or native instructions are included in the repository.

## Missing bounds bypassed assignment

`BSLeafAnimNode` roots were unparsed, so the source-sphere walk returned unknown. Gallows Rock
ferns therefore used the conservative draw-always policy. `BSOrderedNode` parents and empty
`BSBlastNode` children also interrupted the bound walk in effect models.

The converter now parses leaf nodes as node hierarchies, merges ordered-node child spheres, and
accepts zero-sphere empty blast nodes. Nonempty damage ranges remain unsupported. An ordered
node's explicit sort bound replaces its child bound only when both its static-bound byte and
fixed-bound flag enable that native behavior.

The format definitions are in [NifTools' schema](https://github.com/niftools/nifxml/blob/develop/nif.xml).
Exact-build RTTI and vtable inspection independently confirmed that `NiNode`, `BSLeafAnimNode`,
and `BSOrderedNode` use bound-update RVA `0xee2920`. Ordered downward update RVA `0x1570720`
then applies the explicit-bound override under the two conditions above. Serialized node layouts
alone would not establish those runtime rules.

Live probes of fern references `00042152`, `00042179`, `00042184`, and `00042185` found them
under native `BSMultiBoundRoom` nodes. Their converted spheres, transformed by the live native
root transforms, agree with native world bounds within 0.000123 Creation units.

## Activators with usable bounds follow room assignment

The conspicuous white web in the distant Bleak Falls Barrow view includes activators
`000800D5` and `000AA83F`. Skyrim attaches the first to a room and the second to a
`BSPortalSharedNode`. Mudcrab previously exempted every activator and withheld bounds from
every declared skin.

Activators now use the same supported, moving-root assignment policy as loose objects. Legacy
graphs retain their exemption. Self-contained skins can retain initial source spheres when their
raw physics data is fixed. External or unresolved rigs, simulated/keyframed skin bodies,
constraints, unknown bound hierarchies, and independently moving children keep the conservative
fallback. Actors and animated doors retain their separate exemptions.

The physics check reads original rigid-body fields independently of collision export. The web
door has motion system 5, quality 0, and mass 0, classified as fixed; its collision layer 20 is
not exported by the physics reader. Treating every collision-export omission as an unknown pose
would unnecessarily discard its sphere. Conversely, a skipped collision layer must not conceal
a moving bone body.

Nine skinned web references across the door and round-wall models match live native spheres
within 0.000143 Creation units. The web-door model radius is 241.229568, including the native
scale of each placement. Physics-deformed corpse models differ substantially from their source
poses and deliberately receive no source-sphere assignment metadata.

## Rendered verification

The private fixture reconverted 989 models without conversion failures. Source bounds increased
from 2,799 to 2,842 base records. A named mesh-output contract invalidates earlier cached GLBs;
normal asset reconversion is required to recover the new bounds. The installed asset pack was
not modified.

Six camera poses use 1400 × 1030 captures and the native camera's measured 75° horizontal FOV.
Four reproduce the earlier comparison. Two additional native close-ups show the affected web
and fern where they should remain visible. Three independent runs per Mudcrab mode produced
36 captures. All settled without timeouts or pending/failed room graphs.

| Pose | Persistent changed pixels, before versus fixed culling | Fixed culling versus full rendering |
| --- | ---: | ---: |
| Bleak distant shell | 14099 | 322172 |
| Bleak joined-room cave | 0 | 0 |
| Gallows outside, facing away | 11410 | 359811 |
| Gallows inside, facing out | 10764 | 208435 |
| Visible web close-up | Not compared with the old engine | 0 |
| Visible fern close-up | Not compared with the old engine | 0 |

These counts compare Mudcrab images across all nine repetition pairs at a per-channel threshold
of 8. They do not measure speed or RGB equality with Skyrim. The full-render control changed
by one persistent pixel in the Bleak shell view and zero in the other three historical views.
The visible geometry removals therefore come from culling, rather than changing the mesh pack's
rendered content. All 989 final mesh hashes were rechecked after tightening rig eligibility;
none changed from the captured pack.

Regression coverage checks leaf geometry and exported bounds, both ordered-bound flags, empty
versus nonempty damage stages, fixed versus moving bodies on unexported collision layers,
external versus self-contained skins, and moving activator assignment. Validation passed:
1,453 engine tests, 1,206 converter tests, 14 skin-export tests, seven archive tests, and four
snapshots. Forty-three tests remain ignored. Formatting and Clippy for engine/converter, all
targets and features with warnings denied, passed.

## Remaining differences

The close-up still shows corpse pose differences in both culled and full rendering: native
ragdolls have settled, while Mudcrab renders bind poses. Their native bounds cannot be recovered
by labeling an undeformed source sphere as exact. Lighting, fog, water, material brightness,
actors, and other unsupported assignment paths also remain outside this comparison's parity
claim. These fixes establish the targeted web/foliage behavior, not complete Skyrim parity.

Skyrim exited without saving, temporary preferences were restored, the pose batch was removed,
and the private display was stopped.

## Human review recheck, 2026-11-17

The [fresh review comparison](room-assignment-review-20261117.md) supplies matched retail images
for the previously disputed shell views and adds Katariah named-target and NONE coverage. The
[combined frame-cost record](room-culling-combined-review-20261117.md) uses the latest reviewed code
with no local occluders. Its timing ranges overlap, so it does not establish a reliable speedup.
