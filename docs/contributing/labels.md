# Labels

> **Proposal** (#77; a maintainer still has to create the new labels): agreed by a contributor, waiting for a maintainer's confirmation. Follow it for now and comment on the issue if you disagree.

Labels let anyone scan the issue and PR lists without opening each one: what an item touches, what merging it costs players, and whose turn it is. The definitions live in [`.github/labels.yml`](../../.github/labels.yml); a maintainer creates them on the repository (once), and anyone with triage or write access applies them.

## Type

| Label | Use |
|---|---|
| `bug` | fixes something broken |
| `enhancement` | adds a feature or makes something faster |
| `documentation` | mostly docs |
| `proposal` | an issue proposing a direction, open for discussion |

GitHub's default labels stay as they are; `good first issue` and `help wanted` are the ones to search when looking for something to pick up.

## Area

| Label | Use |
|---|---|
| `area: converter` | `crates/converter`, `crates/dummy-content` |
| `area: engine` | `crates/engine` |
| `area: launcher` | `crates/launcher` |
| `area: shared` | `crates/shared`: formats and contracts the other crates agree on |
| `area: ci` | workflows, scripts, tooling |

A PR that touches several areas gets each of them.

## Impact

| Label | Use |
|---|---|
| `needs reconversion` | merging changes the converted output (a converter or database schema bump); players must run a full conversion again (times in [requirements.md](../specs/meta/requirements.md)) |
| `behaviour change` | something that used to work one way now works another way; the PR's "For reviewers" says what |

## Milestone

| Label | Use |
|---|---|
| `needs milestone` | an open issue or pull request with no milestone; a maintainer assigns one ([workflow.md](workflow.md#milestones)) |

The [milestone workflow](../../.github/workflows/milestone.yml) adds this label, comments once with how to fix it, and removes the label when a milestone is set. It creates the label if it is missing.

## Size (PRs)

| Label | Use |
|---|---|
| `size: L` | more than 2,000 changed hand-written lines; consider splitting ([workflow.md](workflow.md#splitting-a-large-pull-request)) |
| `size: XL` | more than 5,000 changed hand-written lines; split before review |

The [PR size workflow](../../.github/workflows/pr_size.yml) sets one of these labels, removes it when the pull request shrinks, and posts the split guidance once. Test and generated files do not count; the full exclusion list is in [scripts/pr_size.py](../../scripts/pr_size.py). The workflow token can label a pull request but cannot create repository labels, so a maintainer creates these two once from [`.github/labels.yml`](../../.github/labels.yml).

## Status (PRs)

| Label | Use |
|---|---|
| `S-ready-for-review` | done, CI green, waiting for a reviewer |
| `S-waiting-on-author` | reviewed; the author has to act |
| `S-blocked` | waits on another PR or a decision; the PR says which |

The author sets `S-ready-for-review` (or asks for it in a comment without triage access); a reviewer switches it to `S-waiting-on-author` after a review, and the author switches it back after answering. Drafts carry no status label.

## Temporary

| Label | Use |
|---|---|
| `stack: <name> <i>/<n>` | every PR of a set of three or more that must merge in order; created for that set and deleted once it has merged. For a single dependency, `**Merge after:** #N` at the top of the PR is enough. |
