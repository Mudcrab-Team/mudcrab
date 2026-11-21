# Implementation design for issue 221

The user requested a full-stop implementation: prepare a proved and reviewed PR, leave it open, and never merge it. At design selection, latest upstream equaled the research baseline, `1dcca42fc398baaf626e588694f9e08084117f11`. There was no existing PR or automatic merge setting. Final integration also includes upstream release/CI commit `ee686282126e369af0b68a8282db80dc7e10dd6a`; the recorded before measurement remains pinned to the original baseline.

## Definition of done

The production WALK transition implements the verified jump-height impulse, first-step gravity and actual-height history; its predictions satisfy the existing held-out tolerance without refitting. Packaged jump inputs come from effective records and have explicit provenance. Real Bevy/Rapier tests prove launch, contact, apex and reset behavior. Existing stairs and dynamic bodies remain correct. The report distinguishes supported semantics, engine policy and unresolved issue items. The final PR is open, mergeable, reviewed, with required checks green and no automatic merge enabled.

## Chosen shape

Three independent sketches were reviewed. The rubric scored native semantics, source validation, scope isolation, proof, interface size and migration (0-2 each). A scored 10/12, B 10/12, C 12/12. A was smallest but retained more exposed scalar state and rejected signed jump heights. B isolated native-unit arithmetic but also rejected signed heights. C is the base because it models vertical ownership and preserves the native absolute-height impulse. A contributes its narrow startup interface and minimal call-site changes. B contributes the native-unit velocity boundary and schedule-only production replay. No different-family judge was available; the configured seats are all gpt-6.1-sol and the other providers were disabled. Parent selection is not an independent cross-family verdict.

Use one private vertical-motion owner, with one authoritative finite jump-height input. Keep Havok-unit velocity inside it and expose Creation-unit velocity/displacement. Airborne state owns maximum resolved height and an optional jump origin. Completed apex is diagnostic state; it never writes the gravity maximum. Do not keep redundant jump-active flags, elapsed time or a configurable launch-velocity scalar when they can be derived or deleted.

The ECS boundary supplies actual resolved body Y and previous KCC support before SyncBackend, before the custom step lift. Launch initializes maximum to current Y and immediately applies the equality-case 1.35 gravity step. Later strict height increases use the base acceleration; equality or lower height uses 1.35. Rapier remains responsible for contact resolution. Preserve the validated custom step sweeps. Existing reset paths reset the whole vertical state; input pauses preserve flight.

Read effective fJumpHeightMin and checked canonical Player NPC_ 7/RACE fields at existing MovementTuning startup. Use ACBS sex, NPC NAM6, RACE DATA heights and normalized RNAM identity. Signed or zero finite GMST is legitimate because native launch uses absolute height. Reject malformed present data and unsupported active trait inheritance. Resolve plugin provenance by load-order priority. Modern complete packages must not silently mix sourced values with synthetic missing factors. Table-less legacy and artificial fixtures have an explicitly reported synthetic unit-actor policy. The exact synthetic height choice is policy, not a native observation; select it after checking the real source inputs. The supported runtime reference scale is one; save mutation/swimming support is not invented here.

Keep production fixed 60 Hz and pass its actual Time<Fixed> delta. Native scheduler ownership is unresolved. Keep Rapier rigid-body gravity at its existing 900 policy, separate from the observed character law. Current evidence does not prove serialized NIF geometry scaling, full MOVT modifiers, or light attenuation. Preserve those numbers and document each issue item honestly; do not close the issue as fully resolved merely because the character change passes.

## Workflow and shared-state boundaries

1. Capture the old production transition and real Rapier jump before replacing behavior. Keep a baseline failure report under the unchanged native tolerance.
2. Extend the native observation tool separately for live jump inputs and exact setter argument. New profile/session/records and declared baseline/setting/reference interventions must preserve every result and the frozen v3 study.
3. Implement the chosen vertical owner and checked package inputs after live measurements establish their meanings. One owner edits coupled engine/fixture/proof files. The native owner edits only recorder files. Parent owns decisions and final documentation.
4. Replay all twelve unique held-out schedules through production Rust code with only its own predicted height feedback. No observed future position, velocity, apex or support enters prediction. No fit or tolerance change.
5. Run real headless Bevy/Rapier launch, floor/ceiling/landing, reset/pause and stair behavior; retain compact numerical proof. Run current CI-equivalent checks and actual runtime inspection as appropriate.
6. Review with sol-loop, fix proven findings, publish the evidence and implementation, then file and babysit the PR. Check live head/base/reviews/checks/mergeability and absence of auto-merge before handoff.

Native game captures use one private session at a time. Parent alone runs the shared Cargo target. Python proof and documentation work can run independently. If measurements contradict the shape, revisit that boundary rather than add compatibility scaffolding.
