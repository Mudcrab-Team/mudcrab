# Writing PRs, issues and review comments

A short guide for anyone sending work here, written by hand or with an AI assistant. It goes with the PR template (`.github/PULL_REQUEST_TEMPLATE.md`); the steps around it (claiming work, drafts, review, merging) are in [workflow.md](workflow.md). The aim is text a reviewer can read in two minutes and a newcomer can follow without already knowing the answer.

## The three rules that matter most

1. **Lead with the result.** The first sentence of a PR says what changes, in plain words, with the headline number if there is one. Mechanism and edge cases come later, or go in a collapsed section.
2. **Explain, don't just state.** For each point, say what goes wrong first, then one concrete example, then what to do and who gains. Explain every Skyrim or engine term the first time it appears; assume the reader knows programming but not Creation Engine formats.
3. **Be honest about evidence.** Tick only the commands you really ran, keep conditions next to the claims they limit ("on Windows", "not runtime-tested"), and give every number its source.

## Claims about Skyrim behaviour

An added comment or doc line that states retail behaviour needs a source in the same place. The source is a retail-facts ID such as `RF-TEX-004`, a link to any repository Markdown file, an external `http(s)` URL, a SHA-256 hash of the analysed file, or the word `unverified`.

The check reads `//` comments in Rust and WGSL under `crates/`, and Markdown under `docs/` and `crates/`. It ignores code and unchanged lines. A moved or renamed line is not a new claim. In Markdown the source can be anywhere in the same section: from a heading to the next heading of the same or higher level. In Rust a `//!` module doc that cites an RF ID labels every comment in that file.

A line that says `non-retail`, `not vanilla` or `Mudcrab-only` still needs a reason, or `unverified`. "Retail" used as an adjective or a label is not a claim: "the retail constant" and "retail value tables" pass, but "Retail profiles hold zero speeds" needs a source.

The accepted reason words are `because`, `since`, `so that`, `to avoid`, `to keep`, `to prevent`, `to match` and `due to`. Naming what Mudcrab does instead also counts (`Mudcrab uses ...`).

The check also reads added rows of the retail-facts registry. A row may carry `kind: rule`, `kind: quirk` or `kind: defect`, and a row without the field is a rule. A row with another kind value fails. A row that says quirk or defect in its text without a `kind:` field also fails. See [ADR-0015](../adr/0015-retail-rule-quirk-defect.md).

Run it locally with `python3 scripts/check_retail_claims.py --base origin/main`. CI runs it in the `scripts` job and prints the file and line for each finding. The gate is required: a finding fails the `scripts` job.

## Voice

- Write concise, full sentences. Cut filler, pleasantries and empty hedges ("maybe", "I think"), but never cut the reason. Headings, field labels and exact commands can stay as shorthand.
- Keep real conditions. "Could", "on Windows" or "not measured on a cold disk" change what a claim means, so they stay right beside it.
- Don't ask others to test or screenshot things for you. Show your own evidence.
- If an AI assistant wrote or helped with the text or the code, say so in one line at the top, for example `> Written with an AI assistant, checked by @you.` Keep it to that one line. See [AI_POLICY.md](../AI_POLICY.md).

## PR titles

PR titles must strictly adhere to the [Conventional Commits](https://www.conventionalcommits.org/) specification (`type(scope): description` or `type(scope)!: description`).

With a squash merge, the PR title becomes the exact commit message on `main` (the repository's squash-merge setting uses the PR title as the commit title). A stack layer lands with a merge commit, which also takes the PR title as its title. Its inner commits come into `main` too, so give them Conventional Commit titles as well. These titles drive changelog generation via `git-cliff` and tell maintainers which SemVer bump a release needs. See [releasing.md](releasing.md). Non-conventional titles are blocked in CI.

- **Format:** `type(scope): plain effect`, at most 72 characters, lowercase after the colon unless the description starts with a listed acronym (for example `fix(engine): NIF parser panic`). Leave out PR numbers and notes like "part 2".
- **Types:**
  - `feat`: New capability, record parser, or feature (maps to *Added*).
  - `fix`: Bug, panic, or visual defect correction (maps to *Fixed*).
  - `perf`: Performance or memory allocation optimization (maps to *Performance*).
  - `refactor`: Structural code cleanup without behavior change (maps to *Changed*).
  - `style`: Formatting, whitespace, or cosmetic code changes without logic changes (maps to *Changed*).
  - `revert`: Reverting a previous commit (maps to *Removed*).
  - `build`: Changes to build system, toolchains, or external dependencies (maps to *Tooling & CI*).
  - `docs`: Documentation, guides, or specifications (maps to *Documentation*).
  - `test`: Adding or correcting tests or fixtures (maps to *Tests*).
  - `chore`: Auxiliary tooling, dependencies, or maintenance tasks (maps to *Tooling & CI*).
  - `ci`: CI workflows, GitHub Actions, and release pipelines (maps to *Tooling & CI*).
- **Scopes:** the allowed scopes (crates such as `converter` and `engine`, and areas such as `ci`, `scripts` and `release`) and the allowed leading acronyms (`NIF`, `LOD`, `BSA`, `MO2` and so on) are listed in `.github/workflows/pr_title.yml`. That file is what CI enforces, so add a scope or acronym there when a new crate or term needs one. A scope is optional when a change crosses several subsystems or applies broadly.
- **Breaking changes (`!`):**
  - Append an exclamation point before the colon for breaking changes: `type(scope)!: description` (e.g., `feat(converter)!: advance world database schema to 10`).
  - In the PR body footer, include `BREAKING CHANGE: <explanation>` detailing migration impact (such as mandatory asset reconversion).

## Release branches

- **`main`**: Target branch for ongoing development and roadmap features.
- **`release/vX.Y.x`** (e.g. `release/v0.1.x`): Maintained for post-release hotfixes and critical patch releases without blocking feature work on `main`.
- Target `main` by default. Only target a release branch when explicitly backporting an approved patch.
- How to tag, publish and hotfix: [releasing.md](releasing.md).

## PR body

Fill in [the PR template](../../.github/PULL_REQUEST_TEMPLATE.md): a one-sentence summary on top, `**Merge after:** #N` if it needs another PR first, then Objective, Details & Implementation (long parts inside `<details>`), Verification & Testing, Visual / Benchmark Proof (leave it out if there is nothing to show), and For reviewers (behaviour change first). The template doesn't say these:

- **Tick only what you ran**, on the final version of the branch. If you ran something narrower or different (one crate instead of the workspace, `--lib` only), leave the box unticked and add a line `Actually run:` with the real command. `cargo nextest run --workspace --all-targets` together with `cargo test --workspace --doc` (nextest skips doctests, see CONTRIBUTING.md) counts as the test box.
- **Length:** aim for about 200 words of prose outside `<details>`, or about 450 for code PRs. Checklists, code blocks and command output don't count. These are targets: go over rather than drop a needed reason, and move mechanism into `<details>` first.
- **Evidence:** for a visual change, before/after pictures from the same command on both builds, cropped to the game window. For a speed change, numbers from a quiet machine, repeated, with the setup stated. For a command-line change, the real output (`$ command`, then what it printed).
- **Pictures:** host them where they won't change (a commit-pinned link, for example `raw.githubusercontent.com/<fork>/<commit>/<file>`), not a link that can move or expire.

### Converter database snapshot

[`world_database_tables_match_snapshot`](../../crates/converter/tests/snapshots.rs) writes every table of a world database built from the synthetic `dummy-content` fixture to a committed snapshot, one sorted line per row. A converter change that alters a published row fails the test and prints a row diff. [`legacy_world_database_tables_match_snapshot`](../../crates/converter/tests/snapshots.rs) keeps the 11 tables the older JSON snapshot covered on the legacy reader path.

A PR that changes either world database snapshot must list each changed row, each added or removed table, and each header change, and say why. To write a reviewed snapshot, run one of these:

```bash
# POSIX shell
INSTA_UPDATE=always python3 scripts/workspace.py cargo test --locked -p converter --test snapshots world_database_tables_match_snapshot
```

```powershell
# PowerShell
$env:INSTA_UPDATE="always"; python3 scripts/workspace.py cargo test --locked -p converter --test snapshots world_database_tables_match_snapshot; Remove-Item Env:INSTA_UPDATE
```

That command updates an existing snapshot. A missing snapshot fails the test, so the update command cannot recreate a deleted file and pass with no review: restore it from Git, or, to create it on purpose, also set `MUDCRAB_SNAPSHOT_CREATE=1` (`$env:MUDCRAB_SNAPSHOT_CREATE="1"` in PowerShell).

Read the diff before you commit it. The snapshot header names every normalized column and every omitted table, and the test fails if that set changes, so no exclusion is silent.

The fixture fills 27 of the 66 tables. These important paths are empty, so the snapshot proves only that the fixture publishes no rows for them: scripts (`scripts`, `script_properties`, `script_property_values`), omissions (`projection_omission_samples`, `projection_omission_stats`, `record_diagnostics`), weather (every `weathers`, `weather_*`, `climates`, `climate_weathers` and `region_weathers` table), water (`waters`), light (`lights`, `cell_lighting`, `lighting_templates`, `emittance`), LOD (`lod`, `lod_build`, `lod_chunks`, `lod_chunks_spatial`) and grass (`grass_types`, `landscape_texture_grasses`). `room_graph_stats` has 12 rows of zeros, and every other room table is empty. A snapshot change on those paths proves reporting only, not world content.

## Draft PRs

Open a draft for anything that isn't ready for review yet: work in progress, or code that works while something is still undecided or unmeasured (a design choice for the maintainers, or a measurement you couldn't do). End **For reviewers** with **Still open (why this is a draft):** what is missing, and what would make it ready. Mark it ready for review once that is done.

## Milestones

Every issue and pull request carries a milestone from [the milestone list](https://github.com/Mudcrab-Team/mudcrab/milestones), so nothing stays uncategorized. Contributors cannot set one: a maintainer assigns it within one working day, and you open the item as usual.

A pull request inherits the milestone of the issues it links with `Closes #N`, `Part of #N` or `Related to #N`, but only when every linked issue that has a milestone agrees. Otherwise it gets the `needs milestone` label and one comment that says what to do. The rule and the daily check are in [workflow.md](workflow.md#milestones).

## Issues

- **Title:** the ask or the problem in plain words.
- **Body:** a one-sentence ask, then why it matters (with an example), then the proposal as bullets, any evidence, and your open questions.
- **Tracking issues** (a list of related work): one sentence saying what the list is for, one line explaining any status words, then checkboxes grouped by area, one line each. Tick an item and add its PR number when it becomes a PR, by editing the list rather than posting a comment for each change.

## Commenting on someone else's work

- **Start with what you support,** specifically, not a bare "+1".
- **Keep three things apart:** what you support, what you would change, and what you have already built (link it). A suggestion is never phrased as a decision.
- **Disagree with reasons:** "I recommend X because Y; the trade-off is Z." No judgements of the person.
- **Questions over demands:** in a review of someone else's PR, number your points and ask. For example, "Could deleted records skip this check?" rather than "Deleted records must skip this check."

## Replying to a review

- Answer each point in the reviewer's order, numbered to match when they numbered theirs.
- Open each answer with **Fixed**, **No change** or **Unresolved**, then give the reason and the commit that has the fix.
- Mention known limits instead of hiding them.
