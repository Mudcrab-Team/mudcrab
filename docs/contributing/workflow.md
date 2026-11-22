# Contributor workflow

> **Proposal**: written by a contributor (it collects the proposals in #77, #78, #79 and #100 and the claiming rules discussed on Discord), waiting for a maintainer's confirmation. Follow it for now, and comment on the issue it came from if you disagree.

How a change goes from an idea to `main`, for people and for AI agents working on their behalf. The goal is that nobody builds the same thing twice and every PR is easy to review.

## 1. Check before you start

Search the open issues **and** open pull requests for the area you want to work on, including who is assigned:

```bash
gh issue list --search "lod in:title,body" --state open --json number,title,assignees
gh pr list --search "lod" --state open --json number,title,assignees
gh issue view <number> --comments   # claims made in a comment only show here
```

If someone is assigned to an issue, has claimed it in a comment, or has an open PR in the same area, comment there before you start. A short "I'd like to help with X" saves a duplicate PR.

## 2. Claim the work

- **Find or open an issue** for what you plan to do. Use the issue templates; a feature gets a short proposal first if it is large or changes how something works.
- **Assign yourself.** If you have triage or write access, use "Assign to me". If you don't, GitHub won't let you assign yourself, so comment "I'm taking this" on the issue instead: that comment counts as your claim, and a maintainer can add the assignment later.
- **Large features** get one umbrella issue, with the phases as separate issues, each assigned to whoever builds it.
- Work you have built but not sent yet can go in one issue listing those branches, so others can see it. Each item leaves the list once it has its own issue or PR.

### Milestones

Every issue and pull request carries a milestone. The [milestone list](https://github.com/Mudcrab-Team/mudcrab/milestones) is the roadmap, so nothing stays uncategorized.

Contributors cannot set milestones. A maintainer assigns one within one working day after an item is opened; you open the issue or pull request as usual. A pull request with no milestone inherits the milestone of the issues it links with `Closes #N`, `Part of #N` or `Related to #N`, but only when every linked issue that has a milestone agrees. Anything else gets the `needs milestone` label and one comment that says what to do. The [milestone workflow](../../.github/workflows/milestone.yml) removes the label when a milestone is set, and its daily run fails while an open item has none. The workflow never checks out pull request code.

## 3. Build

- Branch from the latest `main`, one topic per branch.
- Run what CI runs before you push (see "Run What CI Runs" in [CONTRIBUTING.md](../../CONTRIBUTING.md)).
- If a run exits 0 but the system diagnostic reports show a GPU fault, use [Finding a Metal GPU fault](gpu-faults.md) to find the failing pass.
- Keep a PR to one change a reviewer can hold in their head. Split a large feature into PRs that each work on their own.
- **Inventory tables and stores:** each table has its own file under
  `tools/world_data_inventory/tables/`, and each external store its own file under
  `tools/world_data_inventory/stores/`. Add or edit only the file for your table or store;
  do not add a shared `tables` or `stores` block to `dispositions.json`. Two PRs that add
  different tables or stores then merge without a conflict. A store file records its last
  human review in `reviewed_at_commit`; that commit is provenance only and the gate does not
  check reachability. See
  [data-inventory/README.md](../data-inventory/README.md#migrating-a-branch-that-adds-a-store).

### Splitting a large pull request

One feature per pull request. A mixed pull request hides defects behind its description, and a reviewer cannot hold it in their head.

- **Independent parts** become separate pull requests against `main`.
- **Dependent parts** become a stack: each layer targets the branch below it. A set of three or more carries the same `stack: <name> <i>/<n>` label (see [labels.md](labels.md)); a single dependency uses `**Merge after:** #N` at the top of the PR. Land the stack bottom-up with merge commits, as [step 6](#6-merge-maintainers) describes.
- **Test and generated files do not count** towards the size. The [PR size workflow](../../.github/workflows/pr_size.yml) skips test code, fixtures, snapshots, vendored code, recorded evidence and generated data; the full list is in [scripts/pr_size.py](../../scripts/pr_size.py). It labels a pull request `size: L` above 2,000 remaining lines and `size: XL` above 5,000, then comments once with this guidance. The label is advisory and does not block a merge.

## When `main` moves

Run `python3 scripts/sync_branch.py`. It merges `origin/main` with a merge commit and never
rebases or forces. It resolves only the known mechanical conflicts: generated inventory
files that `main` deleted, `reviewed-surface.json` (take main's side, then regenerate), and
the sorted `Cargo.toml` workspace members. It then runs the fast gates. It stops and lists
the paths for any real conflict. Add `--dry-run` to see the plan first. Never hand-resolve a
known mechanical conflict; a hand resolution can drop a table classification or a reader.

If `dispositions.json` and `reviewed-surface.json` conflict together, resolve
`dispositions.json` by hand first and keep valid JSON. Then run
`inventory.py --review-sources` and `inventory.py --check` against the resolved tree. Stage
`reviewed-surface.json` and commit. `dispositions.json` is human-authored; only the surface
comes from the tool. The script leaves the surface conflicted until the source conflicts are
resolved.

## 4. Open the pull request

- Fill in the PR template, and write the text as [writing-prs.md](writing-prs.md) describes: the result first, the problem with an example, honest evidence.
- **Draft or ready:** open a draft for anything that isn't ready for review yet: work in progress, or code that works while something is still undecided or unmeasured. End "For reviewers" with **Still open (why this is a draft):** what is missing, and mark it ready for review once that is done.
- **Link the issue:** `Closes #N` when the PR finishes it, `Part of #N` when it only helps.
- **Milestone:** a pull request inherits the milestone of the issues it links when they agree; otherwise a maintainer assigns one within one working day (see [Milestones](#milestones)).
- **Labels:** apply the labels in [labels.md](labels.md) that fit. If one doesn't exist on the repository yet, use the closest one that does; a maintainer creates the rest. Setting labels needs triage access: without it, name the labels you'd use in the PR and a maintainer adds them.
- **Depending on another PR:** avoid it where you can. If you can't, put `**Merge after:** #N` at the top and tell reviewers which commits to read.
- **How many at once:** keep about five PRs open at a time, so review keeps up.

## 5. Review

- CodeRabbit reviews every non-draft PR automatically. Answer each of its comments: fix it, or say why not, then resolve the thread. Unresolved threads block the merge.
- Get an **Approve** review from another human collaborator listed in [CODEOWNERS](../../.github/CODEOWNERS) (see [GOVERNANCE.md](../../GOVERNANCE.md) for who that is). CodeRabbit's review and passing CI do not replace this approval. A comment or a resolved thread does not count as approval.
- Answer human reviewers point by point, in their order: **Fixed** (with the commit), **No change** (with the reason), or **Unresolved**.
- If `main` moves while your PR is open, you do not have to update your branch: the ruleset does not require it to be up to date. Merge `main` into it (or use the "Update branch" button) only to resolve a real conflict or to get something your change depends on. Don't rebase or force-push a branch that is under review.
- Changes pushed after approval need another review: stale approvals are dismissed. The ruleset does not require someone other than the last person to push to approve the latest push, so a code owner who pushed can approve the result.

## 6. Merge (maintainers)

The `main_protection` ruleset requires at least one approving review, approval from a human code owner, passing `tests_pass` and `CodeRabbit` checks from their respective GitHub Apps, and every review thread resolved. It dismisses stale approvals when new commits are pushed. Three things are *not* enforced, and an earlier version of this page said they were:

- **An up-to-date branch is not required.** Strict status checks are off, so a PR that is behind `main` can still merge, and a merge to `main` does not force any other PR to be updated or re-approved.
- **Approval of the latest push by someone other than the pusher is not required.** A code owner who pushed to a PR can approve it.
- **The rules have one bypass actor.** Organization administrators (`OrganizationAdmin`, always) can bypass the ruleset. The requirements above apply to everyone else.

`main` accepts merge, squash or rebase merges in the ruleset. The repository setting allows squash and merge commits. Linear history is not required, so that a stack can land with merge commits. There is no merge queue and auto-merge is off.

The PR author may merge after another human collaborator approves and all checks pass. Keep [CODEOWNERS](../../.github/CODEOWNERS) in sync with access changes, using the checklists in [GOVERNANCE.md](../../GOVERNANCE.md#joining-the-project). Its catch-all rule also covers changes to CODEOWNERS itself. Administrators can still edit repository settings, so changing or disabling these requirements is a separate policy change.

- Use **Squash and merge** for every PR that is not a stack layer. Keep the PR title as the commit title.
- Use **Create a merge commit** only to land a stack (a set with a `stack:` label, see [labels.md](labels.md)), bottom-up. Each layer's head commit is an ancestor of the next layer, so a merge commit lands the next layer without a conflict. A squash hides those commits, and the next layer conflicts with its own base.
  1. Merge the bottom PR with **Create a merge commit**. The merge commit takes the PR title and body, so the title must pass the title check.
  2. Make sure that the next PR now targets `main`. GitHub retargets it when the merged branch is deleted. If it does not, change the base to `main`.
  3. Merge the next PR the same way, after its checks pass on the new base. Continue to the top of the stack.
- Do not use rebase merge for a stack. It rewrites the commits, so the next layer conflicts again.
- Squash description: the PR's one-sentence summary, an empty line, then one trailer line per co-author or assistant of the PR's commits (`Co-authored-by:` or `Assisted-by:`, see [AI_POLICY.md](../AI_POLICY.md)). Drop the "Merge branch main" lines and the list of commit titles.
- After a merge, the other open PRs are behind `main` but can still merge without being updated. Update a branch only when it conflicts or needs something that just landed.

## AI agents

Agents follow the same steps, plus [AI_POLICY.md](../AI_POLICY.md). In particular, an agent searches issues and PRs before starting (step 1), never claims or posts without its human's OK, and says in one line that it wrote or helped with a text.
