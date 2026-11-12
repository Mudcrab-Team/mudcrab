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
Property accessors use bare `get_Property` / `set_Property` keys.

The shared helper is `crates/shared/src/papyrus_runtime.luau`. Conversion stages
that source as `scripts/papyrus_runtime.luau`; the Rust host helpers in
`crates/engine/src/papyrus_runtime.rs` embed the same source. No production world
event scheduler or VMAD instance binding is provided by these helpers.

## State dispatch contract

`Runtime:call_method(instance, name, args)` tries the current state's qualified
function, then the empty-state function, then repeats that search in registered
parent scripts. It preserves the original receiver when invoking inherited code.
The existing external `target:dispatch` adapter is the final fallback. Missing
ordinary methods raise an invocation error identifying the script and method.
Parent cycles are diagnosed instead of looping indefinitely.

Emitted `Parent.Method()` calls pass their defining class's parent name to
`Runtime:call_parent`. A parent's own Parent call therefore advances to its own
parent, even on a derived receiver. Static calls and property accessors remain
outside state selection. Parent construction, inherited native ownership and
automatic inheritance of a parent's auto state are not implemented here.

The current resolver preserves exact state/function spelling. The precedence
of a child's empty-state method over a parent's named-state method follows this
implementation's contract; CK inheritance precedence was not independently
verified. These limits must not be described as complete Papyrus parity.

`GetState()` returns the instance's current state name. `GotoState(name)`:

1. Validates the destination against declared states in the script hierarchy,
   including the empty state, before invoking any script code.
2. Invokes the old state's `OnEndState()` if present, using normal state/empty/
   parent fallback while the old state is still visible.
3. Sets the instance state to the destination.
4. Invokes the new state's `OnBeginState()` if present, using the same fallback
   while the new state is visible.

Same-state calls send both callbacks. Missing lifecycle handlers are optional.
Skyrim callbacks take no arguments; Fallout callback signatures are separate.
When callbacks do not themselves change state, an end-callback error leaves the
old state and a begin-callback error leaves the new state. Script side effects are not rolled back. Nested transitions are not
given a special queue or cancellation policy by this synchronous helper.

Unknown destinations raise a per-invocation error. This is Mudcrab's requested
validation policy: CK's OnBeginState documentation also describes undefined
destinations, so rejection is an intentional compatibility limitation.

Invocation errors propagate through `mlua::Result`; callers can report a failing
script and continue invoking others. There is no global error handler or world
scheduler here. The sandbox memory bound and VM-lifetime instruction interrupt
budget are shared by that Lua VM; ordinary-error isolation does not promise
recovery after memory exhaustion or budget exhaustion.

## Sources and verification limits

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

Only OnBeginState was retrievable during this change's primary-source research.
The other links identify the intended rules; their ordering, inheritance and
end-event details remain unverified against retrieved primary text. The sequence
above is the explicit runtime contract covered by synthetic regressions, not a
claim of original-game differential validation.

## Converted output compatibility

State metadata, explicit initial state and lexical Parent calls advance the
script emission ABI to 2. New configurations select the current ABI. Full
conversion raises saved older ABI values to the current minimum before cache
selection, so unchanged PEX inputs regenerate old scripts. Existing per-kind
proofs preserve unrelated eligible mesh and texture outputs. Mesh/texture schema
versions are unchanged. Converted packs need reconversion; rebuilding only the
engine cannot add metadata to old emitted modules.

## Future work

World event delivery, game native bindings, latent continuation scheduling,
save-state restoration, source compilation and IDE type-definition export remain
separate integration work. The current helpers and synthetic tests do not
establish live gameplay, retail semantic parity or performance claims.
