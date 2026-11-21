# Papyrus dispatch and continuation probes against Skyrim

Date: 2026-11-17. Reference executable: Skyrim Special Edition 1.7.104.0 on Linux through Proton Experimental. The executable's fixed version resource identifies that version. Its SHA-256 is `846efccf0c1374d71f892907f46549560f2fcb0a75cb87a3eed438baa0f1402f`.

The live comparison found and fixed one dispatch bug. Skyrim searches the active state through the inheritance chain before trying empty-state methods. Mudcrab previously tried each class's empty state before moving to its parent. Both nested `GotoState` probes already agree with Skyrim. The current shared helper still requires a host provider for `Utility::Wait`; this work does not add a production scheduler.

[Machine-readable evidence](papyrus-parity-20261117.json) records the normalized native traces, helper traces before and after the change, input hashes, and validation results. [Probe sources and reproduction steps](../../tools/papyrus-parity/README.md) are included in the repository.

## Measured comparison

| Case | Skyrim observation | Helper before | Helper after |
| --- | --- | --- | --- |
| Inheritance | Parent and grandparent active-state methods beat nearer empty-state methods. Inherited virtual calls retain the derived receiver. Nested `Parent` calls advance through the lexical ancestry. | Two lookup results differ. | Complete trace matches. |
| Nested call from `OnEndState` | The guarded inner transition runs its lifecycle callbacks, then the outer transition continues to its requested final state. | Complete trace matches. | Complete trace matches. |
| Nested call from `OnBeginState` | The inner transition leaves the object in its new state when the outer begin handler returns. | Complete trace matches. | Complete trace matches. |
| Latent call | `Inner` suspends at `Utility.Wait`; it logs completion and returns 42 before `Outer` continues. | Stops at unregistered `Utility::Wait`. | Still stops at unregistered `Utility::Wait`. |

The table uses the original fresh-game capture of the four guarded quest scripts. A second capture with the corrected quest plugin reproduced every marker. Order is compared separately per quest. The quests initialize concurrently, so their global interleaving is not an expectation. No elapsed-time or scheduling-fairness claim is made.

## Inheritance lookup needed two passes

The fixture has three classes, with the child initially in `Ready`. It distinguishes the active-state search from the empty-state search using values returned by each candidate definition:

| Probe | Skyrim | Previous helper |
| --- | --- | --- |
| Child empty-state method versus parent `Ready` method | `parent.ready` | `child.empty` |
| Parent empty-state method versus grandparent `Ready` method | `grand.ready` | `parent.empty` |
| Parent-only method in `Ready` | `parent.ready` | `parent.ready` |
| Method found only in the grandparent's empty state | `grand.empty` | `grand.empty` |
| Virtual call made by inherited code | `child.ready` | `child.ready` |
| Child, parent, and grandparent lexical `Parent` chain | `child/parent/grand` | `child/parent/grand` |
| Child method after switching to the empty state | `child.empty` | `child.empty` |

`Runtime:find_method` now searches the active-state key across the ancestry, then searches the unqualified key across the ancestry. The receiver and lexical-parent handling are unchanged. An empty active state needs only one search. Cyclic malformed ancestry remains bounded; a known local fallback can still execute, while unresolved lookup reports the existing cycle error.

The existing `pex_parent_dispatch_preserves_receiver_and_defining_class` regression expected the incorrect child-first fallback. Its expectation now follows the native observation, and it also covers the grandparent-state counterexample. The corrected expectation failed before the runtime change. All nine tests in `papyrus_states` pass afterward, including lifecycle-error isolation and script-cache rebuild checks.

The runtime helper ships in each converted pack. Script ABI 4 makes both normal conversion and `--reuse-assets` upgrade packs containing the ABI 3 helper. With matching source data, the metadata rebuild regenerates scripts and installs the corrected helper while retaining verified meshes and textures.

## Nested state transitions matched the observed re-entry

The `OnEndState` fixture uses a boolean guard to permit exactly one recursive transition. Skyrim's normalized trace is:

```text
begin
end.enter=Start
end.enter=Start
end.leave=Start
nested.begin=Nested
end.leave=Nested
final.begin=Final
after=Final
done
```

The `OnBeginState` fixture's trace is:

```text
begin
final.enter=Final
final.end=Final
nested.begin=Nested
final.leave=Nested
after=Nested
done
```

Both helper versions produce these sequences. No transition queue or speculative re-entry rule was added. The observations cover synchronous nested calls within these scripts. They do not establish behavior for another stack entering the same object, lifecycle handlers that suspend or fault, or state changes across a save/load boundary.

## Latent ordering needs an explicit host contract

Skyrim produces this sequence around the nested wait:

```text
begin
outer.before
inner.before
inner.after
outer.after=42
done
```

The primary helper run records the first three markers, then reports `unregistered Papyrus native: Utility::Wait`. The shared helper is a dispatcher; it does not itself provide the timer or scheduler. A successful trace comparison therefore cannot be claimed for this case.

A separate probe replaces the `Utility.Wait` class method with a Luau coroutine yield, inspects the yielded token, and resumes the same thread. It produces one wait token with argument 0.5 and the same complete event sequence as Skyrim. Before resumption, the trace ends at `inner.before`. The value 42 reaches `Outer` after `Inner` finishes.

That adapter is deliberately outside the primary result. It demonstrates continuation order under an explicit, probe-only Luau yield. It does not test yielding across a Rust native callback, register a production `Utility::Wait`, implement clock domains, schedule competing stacks, or prove integration with Bevy. Those are the remaining latent-call requirements.

## Capture and provenance

Caprica v0.3.0 compiled six original `.psc` files for Skyrim without release stripping. Its [official release](https://github.com/Orvid/Caprica/releases/tag/v0.3.0) supplies the Windows compiler. The executable SHA-256 is `389fef19a803c36342fa7f733c96b6e27ef10fcd0b171af10427e29cbe153ec6`. The fixture headers are compiler-only declarations of the required native signatures. They are not deployed as replacements for game scripts.

The generated plugin attaches the four cases to startup quests. The game runs its stock executable, with stock masters and the probe plugin, in a copied Proton prefix and separate game directory. No SKSE loader or native plugin is used. `coc QASmoke` starts the fresh test game from the console. The test exits with `qqq` without saving.

The first, unguarded trial received `OnInit` twice for each quest. Its traces are retained as a rejected capture. Every fixture now sets `hasRun` before emitting its first marker. The accepted capture contains exactly one `begin` and one `done` for each case. The comparator rejects the earlier duplicate capture rather than taking a convenient subset.

The disposable profile creates `Logs/Script` explicitly before launch. Console readback established that logging was enabled during an earlier attempt with no output directory, but that attempt produced no usable trace. It is not treated as a semantic result. A clean prefix-specific Wine-server restart and the precreated directory produced the accepted capture.

The Rust runner loads those same six PEX files through the production `ScriptConverter` and the shared runtime helper. Debug output is captured by a stub. The comparator checks the actual deployed executable, plugin, and PEX files against the preparation manifest, then requires the Luau runner's PEX hashes to match. The evidence JSON records source hashes and the two helper hashes, so the pre-fix and post-fix results remain distinguishable.

Raw logs, assembly dumps, compiled outputs, screenshots, and private prefixes are retained in the local `mudcrab-research/papyrus-parity-20261117` cache. The versioned evidence contains only probe markers and derived identities, with no game binaries, saves, credentials, or Bethesda scripts.

## Verification and limits

The focused lookup regression failed before the fix at the active-state ancestry assertion. After the fix, `cargo test --locked -p converter --test papyrus_states` passed all nine tests with exit code zero. Six comparator controls passed. They cover incomplete and duplicate captures, changed bytecode, interleaved quest output, semantic differences, and a missing native that must not be hidden by the probe adapter. Formatting and whitespace checks passed for the changed Rust files and diff.

The final native comparison matches inheritance and both nested-state traces. It intentionally remains a nonzero comparison because the primary latent case is unsupported. The probe-only adapter matches its ordering separately.

On November 19, a fresh Skyrim run used the corrected plugin with five counted items in its header (four quests and one group). Each quest now ends with an empty `NEXT` marker and a zero `ANAM` next-alias ID. The executable and all six PEX hashes matched the original run. The per-quest traces reproduced all 31 markers, including the return value 42 after the wait. The `corrected_plugin_capture` entry in the evidence JSON pins the new plugin and raw log separately from the November 17 capture.

The probe adapter check requires one wait token with argument 0.5 and the exact pre-resume trace. The runner converts and hashes a private PEX snapshot. Profile preparation requires all six named fixtures, rejects output inside its template, checks copied directory ancestry before writing, and replaces copied configuration-file symlinks. The scripts CI job runs eight comparator controls and five profile-preparation regressions. A Rust test covers snapshot isolation. Both evidence writers refused an output path aliased to an input in a CLI check.

This is a bounded parity result for these programs and Skyrim 1.7.104.0. It does not cover inherited instance-variable initialization, properties, arbitrary state combinations, concurrency, persistence, menu/game-time waits, cancellation, or the full Papyrus native API. No full workspace or in-engine Mudcrab gameplay run was performed.
