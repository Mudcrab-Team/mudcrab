# Mutation testing

Mutation testing asks whether the tests would notice a change to the code. A tool makes one
small change at a time, a *mutant*, such as turning `<=` into `<` or replacing a function body
with a constant. Then it runs the tests.

- If a test fails, the mutant is **caught**: the tests prove that line matters.
- If every test passes, the mutant **survived**: no test depends on that behavior.

A survivor is not automatically a bug. It is a question: does this line need a test, or is the
line equivalent to the original, unreachable, or only cosmetic? Issue #833 describes three
tests that passed without testing what they claimed, which is the problem this finds.

`.github/workflows/mutants.yml` runs [cargo-mutants](https://mutants.rs) on a schedule and on
demand. `.cargo/mutants.toml` holds the exclusions. This page explains how to read a report and
how to run one locally.

## Where it runs, and where it does not

- Schedule: Monday 04:17 UTC.
- Manual: Actions, `mutants`, "Run workflow". Leave the `files` input empty for the scheduled
  scope, or give your own space-separated `--file` globs.
- Never on a pull request. One mutant rebuilds the converter and reruns its tests, so a full
  shard takes tens of minutes. That is too slow for a PR gate.

The scheduled scope is the first slice of the reader and recovery code, about 101 mutants:

- `crates/converter/src/records/scanner.rs`, the framing reader, its boundary recovery, and its
  bounded inflation. 97 mutants.
- `crates/converter/src/esm/mmap_reader.rs`, the memory-mapped plugin reader. 4 mutants.

The rest is deliberately out of the schedule. A hanging mutant costs the whole test timeout, and
the framing reader has several such mutants, so the scope has to stay small enough for eight
shards of two hours. To cover more, dispatch a run with the next file, for example
`crates/converter/src/esm/binary.rs` (116 mutants) for the legacy TES4, record and group reader or
`crates/converter/src/esm/load_order.rs` (127 mutants) for load-order recovery. The whole trees,
`crates/converter/src/records/**` and `crates/converter/src/esm/**`, are 4965 mutants and do not
fit one run. Size a wider run from `outcomes.json` first.

The run is split into eight shards with `--shard k/8`. Each job has a two-hour cap, and the run
step stops at 105 minutes, so a shard that reaches that cap still uploads a partial report. Read
the `not tested` count in the summary before you conclude that a shard was complete: it is the
planned mutants that have no outcome yet.

## Read the report

Each shard uploads an artifact named `mutants-shard-<k>`, holding `mutants.out/`:

| Path | Contents |
| --- | --- |
| `missed.txt` | Surviving mutants, one per line. This is the useful output. |
| `caught.txt` | Mutants that a test caught. |
| `timeout.txt` | Mutants whose test run passed the timeout. Treat these as unproven, not caught. |
| `unviable.txt` | Mutants that did not compile. Usually an equivalent change. |
| `mutants.json` | The mutants the shard planned to test, with file, line, column and diff. |
| `outcomes.json` | One entry per finished mutant: outcome, phase timings and process status. Use it to size a scope change. |
| `log/<mutant>.log` | Build and test output for one mutant. |
| `diff/<mutant>.diff` | The exact change that was applied. |

The `mutants-report` artifact merges every shard's survivors into one `surviving-mutants.txt`, and
the `report` job writes the per-outcome counts to its job summary. You do not have to open eight
artifacts to see the list.

For each survivor, in this order:

1. **Add a test** if the line matters and nothing tests it. Open an issue that names the mutant,
   for example `crates/converter/src/records/scanner.rs:188:24: replace + with - in ...`.
   A test that only kills the mutant without asserting the intended behavior does not count.
2. **Add a documented exclusion** if no test can judge the line. Examples are text-only
   formatting, a call that writes to the terminal, or an unreachable arm that exists for a
   corrupted input. Every exclusion in `.cargo/mutants.toml` needs a reason beside it.
3. **Record it as accepted** if the change is genuinely equivalent, such as `*` becoming `/`
   where the other operand is always 1. Put the reason in the issue, not in the config.

Do not edit the report. It is evidence.

## Run it locally

Install the pinned tools, then use the shared build storage. Do not create a `target/` inside a
worktree.

```sh
cargo install --locked cargo-mutants --version 27.1.0
cargo install --locked cargo-nextest --version 0.9.152
```

List the mutants first. Listing does not build or run tests:

```sh
cargo mutants -p converter --file crates/converter/src/records/scanner.rs --list
```

Then run one file. The default copy mode builds in a temporary copy, so the checkout stays clean:

```sh
cargo mutants -p converter \
  --file crates/converter/src/records/scanner.rs \
  --test-tool nextest --timeout-multiplier 2 --jobs 2
```

`--test-tool nextest` matches CI, so a local survivor means the same as a CI survivor. Add
`--shard 1/4` to split a larger scope across four shells. The report is written to `mutants.out/`
in the checkout; it is not tracked, and it should not be committed.

To reuse one dependency build instead of one per copy, use `--in-place`. cargo-mutants then
mutates the checkout and restores each file afterwards, and it refuses `--jobs` in that mode, so
the mutants run one at a time. Point the build at shared storage, because `--in-place` otherwise
writes a `target/` into the worktree:

```sh
python3 scripts/workspace.py exec --isolated mutants --needed-gib 8 -- \
  cargo mutants -p converter --in-place \
    --file crates/converter/src/records/scanner.rs \
    --test-tool nextest --timeout-multiplier 2
```

Guidance for sizing:

- The converter has about 2100 tests. A caught mutant stops at the first failing test binary, so its
  test phase can take seconds. A survivor runs the whole suite, and a hanging mutant costs the full
  timeout, so survivors and timeouts set the cost, not the mutant count.
- The build is the steady cost: each mutant recompiles the converter and relinks its test binaries.
  In copy mode, each job builds its own copy, so the second job pays one full dependency build
  before its first mutant.
- `--jobs 2` is the CI setting. More jobs mean more dependency builds and more memory.
- `--timeout-multiplier 2` gives a hanging mutant twice the baseline test time. Lower it for a
  tighter cap; raise it if a slow but legitimate mutant is reported as a timeout. Read
  `outcomes.json` from a real run before you widen a scope.

## Limits

- The Ubuntu runner only covers code that compiles and runs there. A `#[cfg(windows)]` path shows
  no survivors because it never runs.
- Mutation testing does not prove correctness. It proves that the tests would notice a change.
- A survivor at a line covered by an integration test can still be real if that test only checks
  the exit status. Read `log/<mutant>.log` before you file an issue.
