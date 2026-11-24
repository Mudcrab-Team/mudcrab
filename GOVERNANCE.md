# Governance

How people take part in Mudcrab, what each rank can do, and how people join, move between ranks and leave. The goal is that anyone can tell who may do what, and that nobody holds more access than their rank needs.

## Ranks

| Rank | GitHub access | Binding approval for `main` | What they do |
|---|---|---|---|
| Contributor | none (works from a fork) | no | Opens issues and pull requests. |
| Triage | Triage | no | Labels, assigns, closes and reopens issues and pull requests. Cannot push. |
| Maintainer | Write | no | Pushes branches to this repository, which stacked pull requests need. Reviews. Merges a pull request once a Core member has approved it. |
| Core | Write | yes | Everything a Maintainer does. Their approval is the one `main` requires. Sets direction and decides promotions. |

Each rank includes everything below it.

**Owner** is a hat, not a rank. Owners are the few Core members who also administer the organization and the repository: settings, rulesets, runners, secrets, invitations, and the bypass of the `main_protection` ruleset. Owners are organization owners and the only people with the repository Admin role. Approving and merging never need Owner access, so most Core members are not Owners. The ruleset bypass (`OrganizationAdmin`, always) was added on purpose, to work through a backlog of pull requests.

## Access today, and the plan

The table above is the target. Live access does not match it yet:

- All six direct collaborators of the repository have the **Admin** role. Five of them are also organization owners.
- The organization has no teams. Direct collaborator grants on the repository are the source of truth for who holds which access.
- [CODEOWNERS](.github/CODEOWNERS) lists every collaborator, so every collaborator's approval is binding today. It also lists one handle that has no access to the repository (GitHub ignores that entry), and one person who has left the project.

In practice, every collaborator is Core and Owner today. The plan below moves the project to the table above. Each step is an Owner action, tracked in its own issue. None of these steps is done yet.

1. Core agrees who is Core and who wears the Owner hat.
2. An Owner changes every other collaborator from Admin to Write (Maintainer) or Triage, and removes the organization owner role from people who are not Owners.
3. One pull request changes CODEOWNERS to list only the Core members, and updates the comment at the top of that file to say so.
4. Later, when there are enough people to need them, an Owner creates the teams in [Teams](#teams-planned), moves each person into their team, and removes the direct grants. A final pull request changes CODEOWNERS to `* @Mudcrab-Team/core`.

Until step 4, use direct collaborator grants for every change below. Removing a person from a team does not remove a direct grant.

## How review and merge work

- `main` is protected by the `main_protection` ruleset: one approving review from a code owner, passing `tests_pass` and `CodeRabbit` checks, and every review thread resolved. New pushes dismiss earlier approvals. The details are in [docs/contributing/workflow.md](docs/contributing/workflow.md).
- Only a code owner's approval satisfies the ruleset. After step 3 of the plan, code owners are exactly the Core members. A Maintainer's approval is still welcome and useful feedback, but it is advisory.
- Anyone with Write access (Maintainer or Core) can merge a pull request once the ruleset is satisfied. The author may merge their own pull request. There is no merge queue, auto-merge is off, and the branch does not have to be up to date with `main`.
- A Core member who pushes to someone else's pull request can then approve it, because `require_last_push_approval` is off. So one Core member can push to an open pull request and approve it alone, for example after resolving a conflict. The cost is less independent review of that push. GitHub never lets the author of a pull request approve it, so this does not apply to the author's own pull request. If Core does not want this, an Owner turns `require_last_push_approval` back on and this paragraph is removed.
- Stacked pull requests: a layer that targets another branch is not gated by the ruleset. The layer that targets `main` is.

## Joining the project

A new person can join at Triage or Maintainer. Nobody starts with Admin. Admin comes only with the Owner hat, which needs Core rank first.

- **Triage** is the usual start for someone who helps with issues, labels or reviews.
- **Maintainer** (Write) is the usual start for someone who needs to push branches, for example to build stacked pull requests. Normally this follows the promotion guideline below. A Core sponsor can propose Write from the start for someone whose work Core already knows, such as a developer who brings in code from a related project.

**How a person is proposed.** A Core member sponsors each new person. The sponsor opens an issue titled `Access: <handle> as <rank>`, or comments on one of the person's pull requests. There is no issue template for this yet. The proposal states:

- the GitHub handle, and that the person has agreed to join,
- the rank and the GitHub access it needs (Triage or Write),
- the sponsor, and a short reason with links to the person's work,
- whether the person becomes a code owner (only for Core).

Other Core members have two days to object for Triage, and one week for Maintainer or Core. With no objection, an Owner does the checklist.

**Checklist for adding a person (an Owner does this):**

1. Invite the person to the `Mudcrab-Team` organization as a member, not as an owner.
2. Give repository access at the agreed level. Until teams exist, add a direct collaborator grant on `Mudcrab-Team/mudcrab` with the role Triage or Write. After teams exist, add the person to the team instead.
3. Only for Core: in the same change, open a pull request that adds the handle to every line of [CODEOWNERS](.github/CODEOWNERS), and link it from the proposal. The grant and that pull request belong together. A code owner without Write access is ignored by GitHub, and Write access without a CODEOWNERS entry makes no binding approval.
4. Comment on the proposal with what was granted and the date, then close it.

## Moving between ranks

Promotion happens in the open, in an issue or a pull request, so everyone can see the sponsorship and any objection. The numbers below are starting points, not hard gates; Core can adjust them.

- **Contributor to Triage:** ask in an issue, or a Core member proposes it. Open to anyone who has been helping with issues or reviews.
- **Triage or Contributor to Maintainer:** about three months of steady involvement and several merged pull requests, sponsored by one Core member.
- **Maintainer to Core:** sustained, good judgment on reviews and design, sponsored by two Core members, with no objection from other Core members within a week.
- **Owner hat:** a Core decision, with no objection within a week. Keep the number of Owners small.

A promotion uses the same checklist as adding a person: change the grant, and for Core, change CODEOWNERS in the same change. For the Owner hat, an Owner also gives the person the organization owner role and the repository Admin role. They are already in CODEOWNERS as Core. Removing the hat reverses both and leaves the Core grant.

## Stepping back and leaving

- **Stepping back:** someone with no activity for about six months moves to alumni. They can ask to return at their previous rank.
- **Leaving:** someone who says they are leaving moves to alumni on that day.
- **Alumni** keep their credit in the history and in release notes. They do not keep access.

**Checklist for removing access (an Owner does this):**

1. For alumni and removals, remove the repository grant completely (direct collaborator role, or team membership after teams exist) and remove them from the organization. Remove the organization owner role if they held the Owner hat. Lower a grant instead of removing it only for a rank change that Core agreed, for example Core to Maintainer.
2. In the same change, open a pull request that removes their handle from every line of [CODEOWNERS](.github/CODEOWNERS), if it is there.
3. Unassign them from open issues, and label work they left behind `help wanted` so others can pick it up.
4. Comment on the issue that records the change with what was removed and the date.

## Removal

Removal is a Core decision, for conduct problems under the [Code of Conduct](CODE_OF_CONDUCT.md) or for misuse of access. An Owner removes access at once with the checklist above. The decision and its reason are recorded where the Code of Conduct allows; private reports stay private.

## Teams (planned)

When teams exist, each rank with GitHub access maps to a team in the `Mudcrab-Team` organization: `triage`, `maintainers` and `core`. Contributors have no access and no team. The Owner hat maps to `owners`. From then on, team membership is the source of truth for who holds a rank. CODEOWNERS names `@Mudcrab-Team/core`, so a Core promotion no longer needs a CODEOWNERS change.

## As the project grows

When an area has a clear expert, such as the converter or the renderer, a path-scoped line in CODEOWNERS can name them. In CODEOWNERS the last matching line replaces the catch-all, and an approval from any one owner on that line is enough. So a path-scoped owner must be a Core member, and the line also names Core, for example `/crates/converter/ @Mudcrab-Team/converter-owners @Mudcrab-Team/core`. Otherwise someone outside Core could approve changes to that path alone. Add extra approval requirements only for changes that are controversial, not for everything.
