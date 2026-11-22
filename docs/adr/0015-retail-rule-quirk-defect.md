# ADR-0015: Classify every retail fact as a rule, a quirk or a defect

- **Status:** Accepted
- **Date:** 2026-11-21
- **Amended:** 2026-11-22. Add that an unfinishable quest is a defect. See
  [Unfinishable quest](#unfinishable-quest).

## Context

Mudcrab ports facts about how retail Skyrim SE works. The facts live in the
[retail-facts registry](../research/retail/registry.md). Content and mods depend on
many of them, so Mudcrab must reproduce those facts exactly.

Not every retail behaviour deserves a copy. Some behaviour is a bug in the retail
executable. A copy of a bug can crash, read undefined memory or grow without
bound. The registry did not say which facts are intended and which are not. A
porter had to decide each case alone, and two porters could decide differently.

## Decision

Mudcrab reproduces Skyrim's rules exactly. A rule is a formula, a constant, a
data meaning, an ordering or a selection rule. Content and mods depend on rules.

Every retail fact that Mudcrab ports has one of three kinds.

| Kind | Meaning | Mudcrab action |
|---|---|---|
| Rule | Intended behaviour. | Copy it. |
| Quirk | Odd but observable. Content may depend on it. | Copy it by default in the `skyrim` profile. A fix may exist in `mudcrab` mode. |
| Defect | A crash, undefined contents, memory corruption, unbounded growth, a race, frame-rate-coupled physics or precision loss. | Do not copy it. Fix it and document the divergence. |

### Rule

Copy a rule in every profile. A rule needs no special note. A registry row has the
kind rule unless the row says otherwise.

### Quirk

A quirk is odd, but a player or a content author can observe it. Content may
depend on it. Copy the quirk by default in the `skyrim` profile, as set out in
[visual profiles](../specs/engine/visual-profiles.md).

A fix for a quirk is allowed only behind the visual or quality profile switch.
The fix belongs to `mudcrab` mode. It never changes the `skyrim` profile. The
fix needs its own note that says what it changes and why.

### Defect

A defect is a retail failure. These are defects:

- a crash;
- undefined contents;
- memory corruption;
- unbounded growth, for example save bloat;
- a race;
- physics that depends on the frame rate;
- precision loss.

Do not copy a defect. Fix it in every profile. Document the divergence in the
registry row and in the code comment that implements the fix.

Content may depend on the behaviour of a defect. An exploit or a timing trick is
an example. A fix of such a defect needs evidence of which content touches it. Decide
each case on its own. Until the evidence exists, record the defect as unverified
for content impact and keep the fix local to what the evidence allows.

### Unfinishable quest

A quest that a player cannot finish because of a script or data fault is a defect.
A stage setter with a missing guard is an example. Stages set out of order are
another. The evidence must show the intended outcome. The quest has a reachable
ending in its data. No content relies on the stall.

The fix ships as a content overlay: a plugin override and a converted script,
with provenance. It never changes an immutable pack. The fix must change no state
in a play-through where retail already works.

Each fix states one intent invariant and cites its evidence. A reviewer signs off
on the intent.

Without intent evidence, the behaviour stays a quirk. Mudcrab copies it in the
`skyrim` profile. Any fix is off by default.

Example (facts only): the Windhelm quest MS11 ("Blood on the Ice") has trigger and
stage guards that can stall it. It is a candidate for a fix. Reproduction in
retail is unverified.

### Worked examples

These facts come from research on 2026-11-21. The list gives facts only.

- **Grass draw-group cap of 960.** Slots from 960 up still submit with fade 0.
  This is a rule. Copy it. Lanes that were never written hold undefined contents.
  This is a defect. Never copy it. Write defined contents instead.
- **Interior portal test.** The test reads rotation rows while the corners use
  columns. This is a quirk. Copy it in the `skyrim` profile.
- **Grass fade-in formula.** This is a rule.
- **Object LOD distances.** These are rules.

### Where the community helps

The community's well-known engine-fix and unofficial-patch projects form a
checklist of facts about retail bugs. Study what they fix and why. Never copy
their code. Do not name or link any decompiled code in the repository.

### Stand-ins

A stand-in is a value or an asset that Mudcrab makes where retail ships one.
The retail data or the retail rule is the source of truth; the stand-in is a
gap. A stand-in is not a Mudcrab-mode choice, so it never becomes the default in
the `skyrim` profile.

Every stand-in needs a label and an issue. An owner is optional. The marker
form is:

```text
STAND-IN(#NNN): what it replaces, and which retail data replaces it
STAND-IN(#NNN, @handle): the same, with an owner
```

The marker sits in the same comment block or Markdown section. It needs an open
issue. Add a GitHub handle when one person owns the replacement. A stand-in
with no retail equivalent says `no retail data`.

A Mudcrab-mode feature keeps the rule above: it needs a reason, and it names its
profile or switch. Adding that profile or switch is not a substitute for the
marker when the feature stands in for retail data.

`scripts/check_retail_claims.py` checks added lines and a committed per-file
baseline. See
[writing PRs](../contributing/writing-prs.md#stand-ins-for-retail-data).

## Enforcement

1. **Registry.** A registry row may carry a `kind` field with the value `rule`,
   `quirk` or `defect`. The default is `rule`. The field format is in the
   [registry](../research/retail/registry.md) header.
2. **Lint.** `scripts/check_retail_claims.py` checks added registry rows. A row
   that has an invalid `kind` value fails. A row whose text says quirk or defect
   needs a `kind` field. See
   [writing PRs](../contributing/writing-prs.md).
3. **Research findings.** A skyrim-research finding is tagged "possible defect"
   when a trace shows undefined behaviour, unbounded growth, a race or
   frame-rate coupling. The registry row then gets `kind: defect` after review.

## Consequences

- A porter knows what to copy and what to fix. The choice is no longer personal.
- The `skyrim` profile stays faithful to rules and quirks. It does not inherit
  crashes or unbounded growth.
- Each divergence from retail has a reason in the [registry](../research/retail/registry.md), so a later reader can audit it.
- A defect fix that content depends on is slower, because it needs evidence.
- Existing registry rows have the kind rule by default. A review may reclassify
  an existing row. That review is a follow-up.
