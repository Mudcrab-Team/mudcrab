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

## 3. Build

- Branch from the latest `main`, one topic per branch.
- Run what CI runs before you push (see "Run What CI Runs" in [CONTRIBUTING.md](../../CONTRIBUTING.md)).
- Keep a PR to one change a reviewer can hold in their head. Split a large feature into PRs that each work on their own.

## 4. Open the pull request

- Fill in the PR template, and write the text as [writing-prs.md](writing-prs.md) describes: the result first, the problem with an example, honest evidence.
- **Draft or ready:** open a draft for anything that isn't ready for review yet: work in progress, or code that works while something is still undecided or unmeasured. End "For reviewers" with **Still open (why this is a draft):** what is missing, and mark it ready for review once that is done.
- **Link the issue:** `Closes #N` when the PR finishes it, `Part of #N` when it only helps.
- **Labels:** apply the labels in [labels.md](labels.md) that fit. If one doesn't exist on the repository yet, use the closest one that does; a maintainer creates the rest. Setting labels needs triage access: without it, name the labels you'd use in the PR and a maintainer adds them.
- **Depending on another PR:** avoid it where you can. If you can't, put `**Merge after:** #N` at the top and tell reviewers which commits to read.
- **How many at once:** keep about five PRs open at a time, so review keeps up.

## 5. Review

- CodeRabbit reviews every non-draft PR automatically. Answer each of its comments: fix it, or say why not, then resolve the thread. Unresolved threads block the merge.
- Get an **Approve** review from a Core member (the code owners in [CODEOWNERS](../../.github/CODEOWNERS); see [GOVERNANCE.md](../../GOVERNANCE.md)). CodeRabbit's review and passing CI do not replace this approval, and a Maintainer's approval is advisory. A comment or a resolved thread does not count as approval.
- Answer human reviewers point by point, in their order: **Fixed** (with the commit), **No change** (with the reason), or **Unresolved**.
- If `main` moves while your PR is open, leave your branch alone: it does not have to be up to date. Merge `main` into it (or use the "Update branch" button) only to resolve a real conflict or to get something your change depends on. Don't rebase or force-push a branch that is under review.
- Changes pushed after approval need another review: stale approvals are dismissed, and a Core member must approve the new head. That can be the Core member who pushed it, for example after resolving a conflict.

## 6. Merge (maintainers)

The `main_protection` ruleset requires at least one approving review, approval from a human code owner, a passing `tests_pass` check, and every review thread resolved. `tests_pass` includes a `reviewers_pass` job that waits for CodeRabbit to report success on the PR's latest commit (Greptile's result is logged but does not block). The branch does not have to be up to date with `main`; CI also runs on every push to `main`. It dismisses stale approvals when new commits are pushed. Only Owners (organization owners) can bypass the ruleset. The bypass was added on purpose, to work through a backlog of pull requests. `main` accepts squash or rebase merges through a merge queue (see below).

Anyone with Write access (Maintainers and Core) may merge once a Core member has approved and all checks pass, including the PR author. [CODEOWNERS](../../.github/CODEOWNERS) lists the Core team, so promoting someone to Core is a team change, not an edit to that file. Its catch-all rule also covers changes to CODEOWNERS itself. Only Owners can edit repository settings, so changing or disabling these requirements is a separate policy change.

- Use **Squash and merge**. Keep the PR title as the commit title.
- Description: the PR's one-sentence summary, an empty line, then one trailer line per co-author or assistant of the PR's commits (`Co-authored-by:` or `Assisted-by:`, see [AI_POLICY.md](../AI_POLICY.md)). Drop the "Merge branch main" lines and the list of commit titles.
- After a merge, the other open PRs can still merge without being updated. Update a branch only when it conflicts or needs something that just landed.

### The merge queue and auto-merge

Merging means adding the PR to the merge queue, not merging it directly. GitHub builds a temporary commit of the latest `main`, the PRs ahead in the queue, and your PR, runs CI on it (the `merge_group` event), and moves `main` to that exact commit if it passes. Nobody rebases or updates branches for this: your PR branch is not rewritten, so its approval stays valid. If your PR conflicts with one ahead of it, it drops out of the queue, and you resolve the conflict and get a fresh Core approval.

- **Merge when ready** (or `gh pr merge --auto`) arms auto-merge on a PR: it enters the queue as soon as `tests_pass` is green, a Core member has approved, and every thread is resolved. The author or any Maintainer can arm it, ideally when the PR is opened.
- **A Core approval on a PR with auto-merge armed ships it.** Core reviewers who still have concerns use "Request changes" or a comment, not "Approve".
- A push after approval dismisses the approval, so the PR waits for a fresh one.
- CodeRabbit and Greptile review the PR's head commit and never report on the queue's temporary commit. The `reviewers_pass` job in `tests_pass` waits for them on the PR, and is skipped in the queue run.

### Updating a branch, and stacks

A rebase rewrites every commit of the branch, and the ruleset dismisses approvals when new commits arrive, so a rebased PR needs a fresh Core approval even if the diff did not change. The rebasing Core member's own approval counts.

- You rarely need to rebase: the branch does not have to be up to date with `main`, and the queue replays your changes onto `main` for you. Rebase or merge `main` only to resolve a real conflict.
- Prefer **Update branch** (a merge of `main` into the branch) over a rebase: it adds a commit instead of rewriting the branch.
- A stack must be rebased after its bottom PR merges, so each remaining layer needs a fresh Core approval. Ask for reviews layer by layer as they reach the bottom, not all at once.

## AI agents

Agents follow the same steps, plus [AI_POLICY.md](../AI_POLICY.md). In particular, an agent searches issues and PRs before starting (step 1), never claims, posts, or arms auto-merge without its human's OK, and says in one line that it wrote or helped with a text.

### Suggesting something to work on

When its human has no task in mind, or has just finished one, an agent can offer to look for open work. It only suggests: the human picks, and claiming follows step 2.

```bash
gh issue list --label "help wanted" --state open --json number,title,assignees,labels,updatedAt
gh issue list --label "good first issue" --state open --json number,title,assignees,labels,updatedAt
```

Drop issues that have an assignee, carry `S-blocked`, or have a claim in the comments or an open PR (step 1). Present a short list: issue number, title, and one line on why it fits. Say plainly when nothing is free.
