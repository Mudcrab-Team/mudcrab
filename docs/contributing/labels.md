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
