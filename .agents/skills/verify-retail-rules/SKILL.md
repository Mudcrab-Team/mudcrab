---
name: verify-retail-rules
description: Run and maintain the Kani proofs of the pure retail rules in crates/retail-rules (NPC scale, reference scale, sex index, actor reference rotation). Use when a change touches crates/retail-rules, the engine code that calls it, or the retail-rule registry rows it cites, or when asked to verify, extend or repair these proofs.
---

# Verify the retail rules with Kani

`crates/retail-rules` holds pure retail Skyrim rules as small scalar functions. Each function
cites a registry row (`RF-...`). Kani proofs (`#[cfg(kani)] mod proofs`) check each rule for
every input in a stated range. The engine does not call the crate yet. The actor scale change
(#663) wires it to `placed_reference_scale` and `placed_reference_rotation`. Until then, the
proofs cover the crate only, not the code that runs.

A proof shows **code = registry rule**. It does not show **registry rule = retail game**. Only a
retail value table can show that. Keep the two claims apart in every report.

## 1. Does a change need this skill?

Set `BASE` to the commit you compare with. For a pull request, that is its merge base with
`main`. For a maintenance pass, it is the sync commit in section 6. Then run:

```sh
BASE="$(git merge-base origin/main HEAD)"   # or the sync commit
git diff --name-only "$BASE"...HEAD -- crates/retail-rules .agents/skills/verify-retail-rules
git grep -n "retail_rules::" "$BASE" -- crates/engine crates/converter
git grep -n "retail_rules::" HEAD -- crates/engine crates/converter
```

The skill applies when the first command lists a file, or when a change edits a call site that
the second or third command lists. Compare the two call-site lists. A call site that is in the
`$BASE` list but not in the `HEAD` list stopped calling the crate; if it now computes a rule
itself, that is a coverage loss. Report it. Read the diff, then use section 3 to find the harnesses and unit
tests for each changed function.

## 2. Commands

Kani is pinned to **0.68.0**. Install it once per machine (about 0.8 GiB: the Kani bundle in
`~/.kani` and one pinned nightly toolchain):

```sh
cargo install --locked kani-verifier --version 0.68.0
cargo kani setup
```

In a Mudcrab checkout with shared storage, run every build through the workspace entrypoint
(see `AGENTS.md`), so it uses a leased directory and not a `target/` in the worktree. Use
`scripts/workspace.py` when the checkout has it. Otherwise use the installed copy:

```sh
WS=scripts/workspace.py
[ -f "$WS" ] || WS="$(git rev-parse --path-format=absolute --git-common-dir)/../.mudcrab-local/bin/workspace.py"
export CARGO_BUILD_JOBS=4
LOG="${TMPDIR:-/tmp}/retail-rules-kani.log"   # outside the worktree
# The entrypoint does not accept `cargo kani`, so wrap it with `exec`.
python3 "$WS" exec --isolated verify-retail-rules --needed-gib 3 -- cargo kani -p retail-rules 2>&1 | tee "$LOG"
crates/retail-rules/check-kani-log.sh "$LOG"
python3 "$WS" cargo --needed-gib 3 test --locked -p retail-rules --no-fail-fast
```

A build of this crate needs well
under 1 GiB, so `--needed-gib 3` is a safe estimate. The entrypoint still refuses to start when
free space is below the 40 GiB reserve; then free space or ask the owner of the machine, and do
not lower the reserve. Follow the machine's build gates in `AGENTS.md`, such as load limits.

Without shared storage (for example in CI), run `cargo kani -p retail-rules` and
`cargo test -p retail-rules --no-fail-fast`. To run only some harnesses, add `--harness <name>`
once for each. A full Kani run takes about 5 minutes on a quiet laptop. Three harnesses take most
of it: `placed_npc_reference_scale_keeps_the_retail_grouping`,
`reference_scale_uses_the_retail_grouping` and `sex_index_selects_the_race_height`. Do not use
`--jobs`: Kani then needs terse output, which has no per-check lines for `check-kani-log.sh`.

Kani output is long. Read the result with:

```sh
grep -E "^Checking harness|VERIFICATION:|Complete -" "$LOG"
grep -B1 -A2 "Status: FAILURE" "$LOG"
```

A good run prints `VERIFICATION:- SUCCESSFUL` for every harness and ends with
`Complete - N successfully verified harnesses, 0 failures, N total.`, and `check-kani-log.sh`
exits 0. Always run that check. Kani 0.68.0 also reports SUCCESSFUL and exits 0 when a
`kani::cover!` is `UNSATISFIABLE`, when an assertion is `UNREACHABLE` (for example after a
contradictory `kani::assume`), or when fewer harnesses ran than the crate has. Such a run proves
nothing. The script fails closed on all three:

- The summary must count every `#[kani::proof]` in `crates/retail-rules/src`, with 0 failures.
  For a run with `--harness`, pass the number of harnesses as the second argument.
- Any `UNREACHABLE` check fails unless its location is in the Rust or Kani library
  (`library/core`, `alloc`, `std`, `kani_core` or `kani`). A check with no location fails.
- Any `UNSATISFIABLE` cover fails.

CI runs the same script. Every harness with a `kani::assume` has `kani::cover!` lines that show
its cases are reachable; add one when you add an assume. A legal unreachable check in this crate
also fails the script; fix the code or the harness, not the script.

## 3. Coverage record

| File | Function | Rule | Kani harnesses | Unit tests and retail rows |
|---|---|---|---|---|
| `crates/retail-rules/src/scale.rs` | `sex_index`, `sex_index_of_form_type`, `npc_sex_index` | RF-NPC-005 | `sex_index_is_acbs_bit_zero`, `a_female_npc_gets_the_female_height` | `sex_index_reads_acbs_bit_zero_for_an_npc_only`; `rf_npc_005_sex_index_matches_every_retail_row` (24 rows, `tests/data/RF-NPC-005.tsv`) |
| `crates/retail-rules/src/scale.rs` | `race_height`, `npc_scale` | RF-NPC-004 | `missing_race_gives_the_npc_height`, `missing_npc_height_gives_the_race_height`, `missing_race_and_npc_height_give_one`, `sex_index_selects_the_race_height`, `finite_inputs_in_range_give_a_finite_scale`, `large_finite_heights_can_overflow` (cover) | `npc_scale_table` |
| `crates/retail-rules/src/scale.rs` | `stored_reference_scale`, `reference_scale`, `retail_reference_scale` | RF-REFR-002, RF-CC-004 (same product) | `reference_scale_uses_the_retail_grouping`, `grouping_changes_the_last_bit` (cover), `a_reference_without_an_npc_base_keeps_its_scale`, `stored_scale_is_monotonic`, `one_hundred_percent_is_exactly_one`, `finite_inputs_in_range_give_a_finite_scale` | `stored_percent_table`, `every_stored_percent_is_close_to_its_exact_value_and_monotonic`, `reference_scale_keeps_the_retail_grouping`, `reference_scale_multiplies_only_for_an_npc_base` |
| `crates/retail-rules/src/scale.rs` | `base_form_scale_factor`, `placed_reference_scale`, `retail_placed_reference_scale` | RF-REFR-002 with RF-NPC-004 (form type 0x2B) | `only_an_npc_base_adds_the_npc_scale`, `a_placed_reference_without_an_npc_base_keeps_its_scale`, `placed_npc_reference_scale_keeps_the_retail_grouping` (the whole function for an NPC base, every stored percent) | `only_an_npc_base_form_adds_the_npc_scale`, `reference_scale_keeps_the_retail_grouping` |
| `crates/retail-rules/src/placement.rs` | `reference_rotation`, `placed_reference_rotation` | RF-REFR-001 (form type 0x3E) | `the_heading_is_always_kept`, `an_actor_reference_drops_x_and_y`, `any_other_reference_keeps_every_angle`, `only_an_achr_drops_x_and_y` | `an_actor_reference_keeps_only_its_heading`, `only_the_achr_form_type_drops_x_and_y`, `any_other_reference_keeps_all_three_angles` |

Bounds and assumptions:

- Every harness covers all values of its inputs (all `f32` bit patterns, all `i32` and `u16`
  values), with two exceptions. `finite_inputs_in_range_give_a_finite_scale` limits heights to
  finite values of magnitude 1.0e6 or less; `large_finite_heights_can_overflow` shows that the
  limit is needed. `sex_index_selects_the_race_height`,
  `reference_scale_uses_the_retail_grouping`, `only_an_npc_base_adds_the_npc_scale` and
  `placed_npc_reference_scale_keeps_the_retail_grouping` exclude zero times an infinity, and
  nothing else.
  That product is NaN in IEEE arithmetic and in retail, but Kani reports any NaN that a
  multiplication makes from non-NaN inputs as a failure (`NaN on multiplication`).
- The grouping is proved in steps and as a whole. `sex_index_selects_the_race_height` (NPC
  scale = race height × NPC height) and `reference_scale_uses_the_retail_grouping` (reference
  scale = stored scale × NPC scale) prove the parts for every `f32`.
  `placed_npc_reference_scale_keeps_the_retail_grouping` proves the whole composed function, with
  the base scale taken from every stored `u16` percent (the retail domain). The same whole proof
  over every `f32` base scale passed once in about 8 minutes, so it is not in CI. Two separate
  chains of two multiplications are slow for the solver; keep a harness to one multiplication on
  each side when you can.
- The product grouping `(f32(percent) × 0.01) × (race height × NPC height)` was confirmed
  statically from the retail machine code. A bitwise run of the retail code is still pending.
  A2 (missing `NAM6` = 1.0) and A3 (how `XSCL` becomes a whole percent) are listed at the top of
  `scale.rs`. They are open questions. Do not present them as retail facts.
- `crates/retail-rules/tests/retail_values.rs` checks approved retail rows (at most 50 synthetic
  rows per registry ID, no addresses, no names) from `crates/retail-rules/tests/data/`. A failing
  row is a finding, as in section 4.
- Engine call sites should call `placed_reference_scale` and `placed_reference_rotation`, so that
  the form-type choices and the grouping stay inside the proofs.
- X and Y of an actor reference are positive zero. The registry says only "0"; the sign is our
  modelling choice and is unverified against retail.
- No call sites in the engine are proved. The proofs cover the crate's functions only. Engine
  policy around them (for example, the guard that draws a zero scale as 1.0) is not retail and is
  not proved here.

## 4. A failure: real bug or modelling error?

1. Read the counterexample. Add `--harness <name> -Z concrete-playback --concrete-playback=print`
   to the Kani command in section 2. Kani prints a unit test whose `concrete_vals` are the input
   values, in the order of the harness's `kani::any()` calls, with a comment that shows each value.
   Replay them on the real code: call the function under test with those values in an ordinary
   `#[test]` on a scratch branch, or use an existing unit test that already fails. Do not commit
   the replay unless it adds a useful regression case. If you may not edit the checkout, say that
   the replay is unverified.
2. Compare each input and the output with the registry row the harness cites.
   - The code gives a value that the rule does not allow: **real bug.** Report the harness, the
     inputs, the expected and actual values (as `f32` bits), and the commit that caused it. Do not
     change the code unless the task asks for a fix.
   - The property asks for more than the rule says (for example, it compares NaN payload bits,
     it assumes a range the rule does not state, or Kani's default `NaN on multiplication` check
     fires on zero times an infinity, which the rule allows): **modelling error.** Fix the property, and write
     the old wording, the new wording and the reason in the commit message.
3. Never make a property weaker, add an assumption or narrow a range only to make a failure go
   away. A finding stays a finding until the code or the registry changes.

## 5. Mutation checks

A proof that was never seen to fail proves nothing. After any change to a harness, apply each
mutation that targets it on a scratch commit, run section 2, and confirm the named harness fails
with a counterexample. Then revert and confirm that every harness passes again.

Give each mutant its own build. Use a new `--isolated` label for each mutant (for example
`verify-retail-rules-m13`), or add `--force-build` to `cargo kani`. Then check that the log has
`Compiling retail-rules`. An independent review of #804 shared one label across scratch
workspaces; cargo reused a stale build, and mutants that should fail passed.

| ID | Mutation | Harnesses that must fail |
|---|---|---|
| M1 | In `race_height`, swap `heights.male` and `heights.female` | `sex_index_selects_the_race_height`, `a_female_npc_gets_the_female_height` |
| M2 | In `npc_scale`, drop the `NAM6` factor | `missing_race_gives_the_npc_height`, `sex_index_selects_the_race_height` |
| M3 | In `stored_reference_scale`, drop `* PERCENT_TO_SCALE` | `reference_scale_uses_the_retail_grouping`, `one_hundred_percent_is_exactly_one` |
| M4 | In `stored_reference_scale`, divide by `100.0` instead of multiplying by 0.01 | `reference_scale_uses_the_retail_grouping` |
| M5 | In `reference_rotation`, keep X for an actor reference | `an_actor_reference_drops_x_and_y` |
| M6 | In `race_height`, return 1.0 instead of 0.0 for an index of 2 or more | `sex_index_selects_the_race_height` |
| M7 | In `retail_reference_scale`, multiply the percent by the NPC factor before × 0.01: `(f32(p) × npc) × 0.01` | `reference_scale_uses_the_retail_grouping` |
| M8 | Compute both products of `retail_reference_scale` (percent × 0.01, then × the NPC scale) in `f64` and round once at the end | `reference_scale_uses_the_retail_grouping` |
| M9 | In `reference_rotation`, drop Z for an actor reference | `the_heading_is_always_kept`, `only_an_achr_drops_x_and_y` |
| M10 | In `sex_index`, read `ACBS` bit 1 instead of bit 0 | `sex_index_is_acbs_bit_zero`, `a_female_npc_gets_the_female_height`, the unit test and the retail rows |
| M11 | In `base_form_scale_factor` or `placed_reference_rotation`, test another form type | `only_an_npc_base_adds_the_npc_scale`, `a_placed_reference_without_an_npc_base_keeps_its_scale`, `only_an_achr_drops_x_and_y` |
| M12 | Add a contradictory `kani::assume` (for example `x > 1.0 && x < 0.0`) to any harness | `check-kani-log.sh` (Kani itself still reports SUCCESSFUL) |
| M13 | In `placed_reference_scale`, compute `(base × race height) × NAM6` for an NPC base | `placed_npc_reference_scale_keeps_the_retail_grouping`, the unit test `reference_scale_keeps_the_retail_grouping` |

Who ran each mutation: the pilot ran M1 to M4, M11, M12 and M13. The first review of #804 ran M6
to M9. The cold-run agent found M10. The delta review of #804 ran M5, ran M11 again, and found
M13, which no harness caught until `placed_npc_reference_scale_keeps_the_retail_grouping` was
added. One limit: computing a single product of two `f32` values in `f64` and rounding once
gives the same bits as `f32`. Such a change is not a mutation: no proof detects it, and it is
not a bug. M8 differs, because it rounds once over two products.

A caller that multiplies the three factors itself, in another order, is outside these proofs.
Callers should use `placed_reference_scale`. The unit test `reference_scale_keeps_the_retail_grouping`
pins one input whose two groupings differ in the last bit.

## 6. Sync and maintenance

**Sync commit:** the merge commit of the follow-up to #804 that added
`placed_npc_reference_scale_keeps_the_retail_grouping` (checked on 2026-11-18, on top of `main`
`c895d12d`, the merge of #804).

Move the sync commit forward only after you run section 2 and the mutations for every changed
harness, and confirm that the proofs still describe the code at the new commit. Then:

1. Review every change to a covered file since the sync commit. Classify each one: no effect;
   the proof no longer matches the code; the rule changed on purpose (with evidence, such as a
   registry change); or the rule may be broken.
2. Update a harness from the code and the registry, never from the result. A rule change needs
   the old wording, the new wording and its evidence.
3. Add new rule functions and harnesses to section 3, and new mutations to section 5.
4. Record each check as in sync, updated, bug or blocked. A blocked check keeps its old sync
   commit.

New rules belong here only when they are pure scalar functions. Keep Bevy, glam, database and
file types out of this crate. Ordering questions across systems need TLA+, not Kani.
