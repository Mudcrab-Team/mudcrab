# Production review ledger

The initial sol-loop completed one round with gpt-6.1-sol through the Codex provider. The delegate response exposed no effort setting or token usage; neither is inferred here. All configured design and evidence-review seats were also from the same model family.

| Round | Verdict | Findings | Outcome |
| --- | --- | --- | --- |
| 1 | APPROVE | No blocker, should-fix or nit assigned | No behavior changes required |

The [review](sol-review-round1.md) covers the staged diff against `ee686282126e369af0b68a8282db80dc7e10dd6a`, concentrating on production motion, input loading, migrated callers and replay boundaries. The reviewer reran both evidence rechecks successfully. It did not rerun Cargo, native acquisition or hosted CI, and did not exhaustively inspect every native interception helper.

Before review the diff contained 137 files, 27,303 added lines and 160 removed lines, mostly retained numerical evidence. Review adds only this ledger, the review text and a decision-log entry. No fixes or declines were needed. Required hosted checks and human approval remain separate gates. Publication proceeds under the user's full-stop instruction; the PR must stay open and unmerged.


## Hosted review follow-up

| ID | Severity | Finding | Disposition |
| --- | --- | --- | --- |
| B1 | should | Evidence-only changes skipped CI validators | Fixed filters for native tools, evidence archives and Skyrim Python helpers; executable filter regression fails before and passes after |
| B2 | should | Failed capture rows lacked retained trace identities | Fixed both proof and runner failure paths, explicit absent-trace state, source archival and resume; complete CLI-to-verifier regression passes |
| B3 | nit | Aggregate docstring coverage recommendation | Declined broad docstring edits; the new failure helper has a targeted docstring and frozen evaluator/acquisition snapshots retain their original bytes |

The [second Sol round](sol-review-round2.md) approved these fixes and their evidence. B1 and B2 are resolved, with no new blocker or should finding. It independently passed all 157 Python tests, the historical corpus verifier and the whitespace check. The production Rust transition and original observations remain unchanged. No effort setting or token usage was exposed for either delegate. The second review required no further behavior change.

The follow-up diff entering round 2 contained 15 files, 235 additions and 9 deletions. Approval adds only the reviewer report, this ledger update and the decision entry. Hosted checks and human approval remain separate from both Sol verdicts.


## Attempt-accounting follow-up

| ID | Severity | Finding | Disposition |
| --- | --- | --- | --- |
| B4 | should | Recorder preflight can exit without `failure.json` | Reproduced and fixed with an explicit recorder-error fallback; real preflight is exercised without launching Skyrim |
| B5 | should | Overflow or missing proof fields escape failure retention | Reproduced and fixed at the proof boundary, with original exceptions re-raised |
| P1 | should | An unclassified attempt can disappear when a later retry succeeds | Reproduced by deleting each historical failure marker; verifier now rejects an unmatched attempt ledger |

The [third Sol round](sol-review-round3.md) approved these changes with no findings. B4, B5 and P1 are resolved. It independently passed all 158 Python tests, both historical rechecks, receipt hashes and the staged whitespace check. The source delta is three Python files, 59 additions and 14 deletions before evidence documentation. The complete diff entering review was nine files, 99 additions and 14 deletions. Approval adds only the reviewer report, this ledger update and the decision entry.

Sol-loop is complete after three rounds with gpt-6.1-sol. No effort setting or token usage was exposed. The final round made no behavior changes and did not rerun Cargo or native Skyrim. Hosted checks must cover the final committed head, and required human approval remains separate.

## Maintainer review follow-up, 2026-11-10

The [new review](https://github.com/Mudcrab-Team/mudcrab/pull/544#issuecomment-6761164439) raised six numbered findings and four additional points. Their reproduced mechanisms, fixes and retained policies are recorded in the [response](../review-2026-11-10.md). The fixes cover publication provenance, shared screenshot ownership, achieved cadence, default native replay and input diagnostics. Manifest integrity is pinned independently of the training seal. The predeclared numerical score gates remain unchanged.

The review budget extends to a fourth round because this follow-up changes shared-resource ownership and evidence integrity. The [fourth Sol round](sol-review-round4.md) approved the follow-up with no findings. It independently passed the publication audit, portable historical recheck, 174 Python tests selected by `test*skyrim*.py`, and staged whitespace. The parent ran the full 176-test Python discovery, 106 Rust physics tests, Clippy, formatting, native recorder builds, and fresh production replay. No new native capture is claimed.

The follow-up entered review with 72 files, 1,358 additions and 5,329 deletions. Approval adds only this ledger update, the review report and a decision entry. No effort setting or token usage was exposed. Sol-loop is complete after four rounds with gpt-6.1-sol. Hosted CI and required human approval remain separate gates.


## Final CI follow-up, 2026-11-10

The new-head security job found RUSTSEC-2026-0459 in libsqlite3-sys 0.30.1. The same audit fails on upstream main 8e9a875. Updating rusqlite to 0.41.1 and libsqlite3-sys to 0.39.1 clears that advisory without adding a suppression. The update requires explicit checked unsigned conversions and two small converter API adaptations.

CodeRabbit also identified a v4 session passing the v3 campaign preflight. The regression reached the recorder before correction. The guard now rejects the mismatched profile before writing output, while the historical v3 session still reaches the recorder.

The full workspace run also exposed an unregistered publication-helper option in the engine's script-option audit. The fix scopes four publication options to that helper and adds the new capture test to the existing native-tool path list. The existing scope tests and the complete 3,305-test workspace run pass afterward.

The review budget extends to its fifth and final round for the security correction. The [fifth Sol round](sol-review-round5.md) approved C1, C2 and C3 as resolved, with no new findings. It independently passed all 178 Python tests, the seven source hashes, five JSON artifact hashes, eight published log hashes and staged whitespace checks. Cargo and hosted CI were not rerun by the reviewer. The parent validation is recorded in the [post-CI receipt](validation/post-ci-2026-11-10/receipt.json).

The follow-up entered review with 25 files, 376 additions and 68 deletions. Approval adds only this ledger update, the reviewer report and a decision entry. Sol-loop is complete after five rounds with gpt-6.1-sol. No effort setting or token usage was exposed. Hosted CI must cover the final committed head; required human approval remains separate.
