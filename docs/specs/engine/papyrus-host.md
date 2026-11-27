# Mudcrab Papyrus Host: Running Converted Scripts

How the engine runs the Luau scripts the converter emits from Skyrim's Papyrus scripts. The code is
`crates/engine/src/papyrus_host.rs` (the host, the natives and the world state),
`crates/engine/src/papyrus_plugin.rs` (the Bevy systems), `crates/engine/src/papyrus_runtime.rs`
(the sandboxed VM and its budgets) and `crates/shared/src/papyrus_runtime.luau` (the runtime the
scripts call). Issue #632 sizes the slice.

Scripts are **opt-in**: nothing in this document happens unless the engine is run with
`--papyrus-scripts`. The flag only takes effect in an interactive run (a window with the player), so
`--headless` and `--hidden-window` do not install the plugin.

---

## 1. What runs

**Load.** The host reads `<assets>/scripts/<lowercase name>.luau` the first time a script or class
is named. The runtime built into the engine is used, not the copy the converter writes into the pack
(`scripts/papyrus_runtime.luau`).

**Attach.** When the streamer spawns a reference, the host looks up the scripts on the reference and
on its base record (`scripts`, `script_properties`, `script_property_values`). Base values apply
first and the reference's override them. A script attaches only if its name is in the allowlist
(`SLICE_SCRIPTS`) and its class extends `ObjectReference`. Each attach logs one `info` line.

**Property rows.** `script_properties.status` holds the VMAD property flag, which xEdit names
1 (Edited) and 3 (Removed). The host seeds an Edited row. A Removed row sets nothing (#1052). Each
type needs its own value column: a NULL or out-of-range column, a value count that does not match,
or an undefined status or type makes the property unreadable. The host logs it and does not set it
(#1095). A stored 0, empty string or false is a value. `crates/engine/tests/sqlite_null_guard.rs`
rejects a new NULL-to-literal coercion in the engine's SQLite readers.

**Quest aliases.** An object value with an alias id other than `0xFFFF` names a quest alias (#1051).
A property typed `ReferenceAlias`, `LocationAlias` or `Alias` holds that alias, and every property
bound to it shares one object. `GetReference` reads the current fill. Two cases stay `None` with a
`warn` line: a property typed as a derived alias script (the database has no alias script data), and
a property whose script declares no type for `::<name>_var`. Nothing fills aliases yet.

**Allowlist.** Four scripts: `defaultDisableSelfOnActivate`, `defaultUnlockOnLoad`,
`dunDeadMensRotatingDoor` and `dunPOIDragonNoteScript`. Every other converted script stays detached
until its natives exist.

**Events.** `OnInit`, `OnCellAttach` and `OnLoad` when the reference spawns, and `OnActivate` when
the player presses E on it. A missing handler is skipped. A reference that spawns again after its
cell reloaded gets `OnCellAttach` and `OnLoad` again and keeps its state.

**Time.** `Utility.Wait` and `RegisterForUpdate` run on a scheduler driven by the frame clock. Each
entry into script code is a coroutine, so a wait suspends one script and nothing else.

## 2. Natives

| Class | Function | Behaviour |
| :--- | :--- | :--- |
| Game | `GetPlayer` | the player reference (`0x14`) as an `Actor` |
| ObjectReference | `Disable`, `Enable`, `IsDisabled` | script state, plus an effect the plugin applies |
| ObjectReference | `Lock` | records the lock state; no lock system yet |
| ObjectReference | `GetLinkedRef` | reads `reference_linked_refs` |
| ObjectReference | `AddItem`, `GetItemCount` | per-container counts in memory |
| ObjectReference | `PlayAnimation` | returns false and logs once: no animation graph yet |
| ObjectReference | `AddToMap` | records the marker; no map yet |
| Actor | `IsInFaction`, `SetFactionRank` | ranks scripts set; base-record factions are not read |
| ReferenceAlias | `GetReference` | the current alias fill, or `None`; nothing in the engine fills aliases yet |
| Quest | `Start`, `IsRunning`, `IsStageDone`, `GetCurrentStageID`, `SetCurrentStageID` | in-memory quest state |
| Form, Alias, ActiveMagicEffect | `RegisterForUpdate`, `RegisterForSingleUpdate`, `UnregisterForUpdate` | the scheduler |
| Utility | `Wait`, `WaitMenuMode` | suspend the calling script |
| Debug | `Trace`, `Notification` | log lines |

A native nobody bound logs `Class::Function` once, is counted, and returns `None`
(`PapyrusHost::unbound_natives` ranks them). `Disable` hides the reference and turns its colliders
off, on the entity and its descendants. `Enable` does nothing visible for a reference that starts
disabled: the streamer never spawns it.

## 3. Faults and budgets

A script fault (a call on `None`, a missing method or class) logs once per distinct message and
returns `None`; the script carries on. A script error ends only that entry. Nothing a script does
reaches the engine as a panic or a returned error.

Three guards bound the work. They are engineering guards, not retail values, because the retail
registry has no Papyrus rows: 2,000,000 interrupts for one entry, 10,000,000 for all entries in
one frame, and 64 MiB for the VM. An entry that runs out fails and logs; an entry that starts
after the frame total is spent fails the same way.

## 4. Differences from the original game

These points are unverified: the repository holds no captured source that records them.
- Retail runs scripts in short time slices on the game thread. Mudcrab runs each entry to the end
  inside its budget.
- Retail sends `OnLoad` when the reference's 3D is loaded. Mudcrab sends it at spawn, before the
  model loads. The four slice scripts do not depend on the 3D.
- Retail shows an activation prompt only for activatable references. Mudcrab sends `OnActivate` to
  any scripted reference whose model bounds the E ray hits.
- Retail logs a script error and drops the stack. Mudcrab logs and continues with `None` for the
  faults in section 3.
- Not in the slice: trigger volumes, death and combat events, an animation graph, a lock system, a
  map, quest alias filling, and quest alias scripts.
- When a reference removes a property that its base record sets, Mudcrab keeps the base value. In
  the vanilla pack, 13 of the 18 Removed rows have a base value for the same property.
- The database has no VMAD script status, so a removed script still attaches (#1050).
- Several scripts on one form all attach and run their own events, but a native or another script
  that looks the form up gets only the first script object.
- One E press goes to the nearest scripted reference the ray hits, and a load door or a carryable
  object can react to the same press. A scripted reference with no model bounds cannot be hit by
  the ray (the door path uses a fallback box; this path fails closed).
- A reference the streamer spawned counts as enabled, so `Disable` works whatever its own header
  flag says (an enable parent may have enabled it). For a reference that is not resident, the host
  reads only its own flag and does not follow enable parents.
- Packs: the host needs the `script_properties` and `script_property_values` tables. A pack made
  by the legacy record reader stores properties as JSON instead, so the host refuses it with a
  message and scripts stay off.

## 5. Checking it

Headless tests (`cargo test -p engine --lib papyrus`) drive each allowlisted script through the host
with a fake world, and press E in a Bevy test app. Setting `MUDCRAB_PAPYRUS_SCRIPTS` to a pack's
`scripts` folder runs the same assertions on the real converted scripts. Setting
`MUDCRAB_PAPYRUS_PACK` to a pack folder reads every script property in its database and checks the
alias and removed-property rules on vanilla records. In a live run, look for
`scripts on: attaching ...`, one `attached ... to <form id>` line per scripted reference, and
`script disabled <form id>` after pressing E.
