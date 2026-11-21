# Papyrus PEX to Luau

## Implemented conversion and host helpers

`crates/converter/src/script.rs` parses and verifies Skyrim PEX bytecode, then
emits deterministic `.luau` files. Source `.psc` compilation is a future goal.
Fixtures are generated inside tests; game scripts are not distributed.

Each emitted module returns a class table with `__name`, `__parent`,
`__autoState`, `__states` and variable `__types`. `Script.new(runtime)` creates
an independent instance, initializes variable defaults and sets `__state` to
the class's auto state. An empty auto-state name selects the empty state.
Initial creation does not send state transition callbacks.

Functions in the empty state use `Script["Method"]`; named-state functions use
`Script["State::Method"]`. Events use the same function export and dispatch
mechanism. Declared states with no functions still appear in `__states`.
Property accessors use bare `get_Property` / `set_Property` keys. Emitted
function, property and variable keys retain their source spelling; declared-state
metadata keys in `__states` use ASCII lowercase. Runtime name lookup is ASCII
case-insensitive, including state/function names, locals, instance fields,
property accessors, class/parent identifiers and native/static registrations.
An exact-key fast path falls back to scanning original keys for the same lowercase
name, so hand-written host class tables keep their keys and callbacks. Class and
native registries use lowercase keys. Names differing only by case denote one
Papyrus identifier; malformed duplicate declarations with conflicting bodies are
outside this contract. Data strings are preserved, including GotoState arguments
and GetState results. External dispatch adapters receive the original method name.

The shared helper is `crates/shared/src/papyrus_runtime.luau`. Conversion stages
that source as `scripts/papyrus_runtime.luau`; the Rust host helpers in
`crates/engine/src/papyrus_runtime.rs` embed the same source. No production world
event scheduler or VMAD instance binding is provided by these helpers.

## State dispatch contract

`Runtime:call_method(instance, name, args)` searches the instance's class and its
registered ancestors for the current state's qualified function. If that search
finds no function, it searches the same classes in the same order for an empty-state
function. It preserves the original receiver when invoking inherited code.
The existing external `target:dispatch` adapter is the final fallback. Missing
ordinary methods raise an invocation error identifying the script and method.
Parent cycles are diagnosed instead of looping indefinitely.

Emitted `Parent.Method()` calls pass their defining class's parent name to
`Runtime:call_parent`. A parent's own Parent call therefore advances to its own
parent, even on a derived receiver. Static calls and property accessors remain
outside state selection. Static functions receive a per-call class context with
the invoking runtime, so generated global functions and natives can make further
calls without storing mutable runtime ownership on the registered class. Property
accessors use empty-state and parent lookup. Parent construction, inherited native
ownership and automatic inheritance of a parent's auto state are not implemented here.

In the Skyrim inheritance probe, the parent's active-state method wins over the
child's empty-state method. The grandparent's active-state method also wins over
the parent's empty-state method. See the [retail traces](../../research/papyrus-parity-20261117.md).

`GetState()` returns the instance's current state name. `GotoState(name)`:

1. Accepts a string destination, including an undeclared name or the empty string.
2. Invokes the old state's `OnEndState()` if present, using the method lookup
   described above while the old state is still visible.
3. Sets the instance state to the destination exactly as supplied.
4. Invokes the new state's `OnBeginState()` if present, using the same fallback
   while the new state is visible.

Same-state calls send both callbacks. Missing lifecycle handlers are optional.
Skyrim callbacks take no arguments; Fallout callback signatures are separate.
When callbacks do not themselves change state, an end-callback error leaves the
old state and a begin-callback error leaves the new state. Script side effects are not rolled back. Nested transitions are not
given a special queue or cancellation policy by this synchronous helper.

An undeclared destination is stored and calls use the ordinary empty-state
fallback, including lifecycle handlers. Undeclared auto-state metadata uses the
same fallback during initialization, without lifecycle callbacks. The PEX name
is safely decoded; no compiler invariant requiring rejection has been verified,
so this helper keeps that script usable rather than raising a construction error.
That permissive initial-metadata choice is not original-game differential proof.

Invocation errors propagate through `mlua::Result`; callers can report a failing
script and continue invoking others. There is no global error handler or world
scheduler here. The sandbox memory bound and VM-lifetime instruction interrupt
budget are shared by that Lua VM; ordinary-error isolation does not promise
recovery after memory exhaustion or budget exhaustion.

## Sources and verification limits

- [Identifier Reference](https://ck.uesp.net/wiki/Identifier_Reference),
  [Function Reference](https://ck.uesp.net/wiki/Function_Reference) and
  [Script File Structure](https://ck.uesp.net/wiki/Script_File_Structure): intended
  references for case-insensitive Papyrus identifiers. This well-established rule
  is the required lookup contract; fresh primary retrieval was blocked by HTTP 403.
- [States (Papyrus)](https://ck.uesp.net/wiki/States_(Papyrus)) and
  [State Reference](https://ck.uesp.net/wiki/State_Reference): intended reference
  for state overrides, empty-state fallback and starting-state selection.
- [GotoState](https://ck.uesp.net/wiki/GotoState_-_All_Scripts),
  [OnEndState](https://ck.uesp.net/wiki/OnEndState) and
  [GetState](https://ck.uesp.net/wiki/GetState_-_All_Scripts): intended references
  for transition order, end notification and current-state reporting.
- [OnBeginState](https://ck.uesp.net/wiki/OnBeginState): verified zero-argument
  Skyrim signature, no initial auto-state callback and repeated begin notification
  for same-state GotoState. It also documents undefined destinations.

Only OnBeginState was retrievable during the preceding state-dispatch research;
fresh retrieval, including the identifier references, was blocked during follow-up.
The other links identify the intended rules; their ordering, inheritance and
end-event details remain unverified against retrieved primary text. The retail
probes independently covered active-state inheritance and nested transitions
from both lifecycle callbacks. Synthetic regressions cover the wider contract.

## Converted output compatibility

Script emission ABI 2 added state metadata, explicit initial state and lexical
Parent calls. ABI 3 added case-insensitive generated identifier/field access and
permissive auto-state initialization. ABI 4 packages the corrected inherited
state lookup. With `--reuse-assets`, an ABI 3 pack rebuilds its scripts from
matching PEX sources and stages the current embedded helper. New configurations
select the current ABI.
Full conversion raises saved older ABI values to the current minimum before cache
selection, so unchanged PEX inputs regenerate old scripts. Existing per-kind
proofs preserve unrelated eligible mesh and texture outputs. Mesh/texture schema
versions are unchanged. Converted packs need reconversion; rebuilding only the
engine cannot update identifier helpers in old emitted modules.

ABI 5 changes no emitted script. It marks the shared `papyrus_runtime.luau` that gained
inherited-native lookup, fault-and-continue, a scheduler and execution budgets (#703). ABI 4 was
used for two different runtime contents (the inherited state lookup above and an earlier form of
this change), so ABI 4 packs from either side rebuild their scripts on the merged converter. A pack
at ABI 3 or 4 still loads in the engine, which never reads a pack's ABI or its runtime copy; with
`--papyrus-scripts` it runs the pack's scripts on the engine's built-in runtime. `--reuse-assets`
on such a pack rebuilds the scripts only (no mesh or texture work) and rewrites the runtime copy.

## Future work

World event delivery, game native bindings, latent continuation scheduling,
save-state restoration, source compilation and IDE type-definition export remain
separate integration work. The current helpers and synthetic tests do not
establish live gameplay, retail semantic parity or performance claims.
