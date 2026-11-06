# Governance

How people take part in Mudcrab, what each level of involvement can do, and how someone moves between levels. The goal is that anyone can tell who may do what, and that nobody holds more access than their role needs.

## Ranks

| Rank | GitHub access | Binding approval for `main` | What they do |
|---|---|---|---|
| Contributor | none (works from a fork) | no | Opens issues and pull requests. |
| Triage | Triage | no | Labels, assigns, closes and reopens issues and pull requests. Cannot push. |
| Maintainer | Write | no | Pushes branches to this repository, which stacked pull requests need. Reviews. Merges a pull request once a Core member has approved it. |
| Core | Write | yes | Everything a Maintainer does. Their approval is the one `main` requires. Sets direction and decides promotions. |

**Owner** is a hat, not a rank. Owners are the few Core members who also administer the repository and organization: settings, rulesets, runners, secrets, and the ability to bypass the `main_protection` ruleset. Approving and merging never needs Owner access, so most Core members are not Owners. The bypass was added on purpose, to work through a backlog of pull requests.

Each rank includes everything below it.

## How review and merge work

- `main` is protected by the `main_protection` ruleset: one approving review from a code owner, a passing `tests_pass` check, and every review thread resolved. New pushes dismiss earlier approvals.
- [CODEOWNERS](.github/CODEOWNERS) names the Core team, `@Mudcrab-Team/core`. Only a Core approval satisfies the code owner requirement. Until CODEOWNERS points at that team, it lists Core members by handle, and promoting someone to Core also adds their handle there. A Maintainer's approval is still welcome and useful feedback, but it is advisory.
- Anyone with Write access (Maintainer or Core) can send a pull request through the merge queue once the ruleset is satisfied, or arm auto-merge so it enters the queue as soon as it is. The author may do this for their own pull request. A Core approval on a PR with auto-merge armed ships it. A Core member who pushes to a pull request can approve it.
- Stacked pull requests: layers that target another branch are not gated by the ruleset. The layer that targets `main` is.
- The step-by-step path from idea to merge is in [docs/contributing/workflow.md](docs/contributing/workflow.md).

## Moving between ranks

Promotion happens in the open, as an issue or a pull request, so everyone can see the sponsorship and any objection. The numbers below are starting points, not hard gates; Core can adjust them.

- **Contributor to Triage:** ask in an issue. Open to anyone who has been helping with issues or reviews.
- **Triage or Contributor to Maintainer:** about three months of steady involvement and several merged pull requests, sponsored by one Core member.
- **Maintainer to Core:** sustained, good judgment on reviews and design, sponsored by two Core members, with no objection from other Core members within a week.
- **Stepping back:** someone with no activity for about six months moves to alumni. They can ask to return at their previous rank.
- **Removal:** a Core decision, for conduct problems under the [Code of Conduct](CODE_OF_CONDUCT.md) or for misuse of access.

## Teams

The ranks with GitHub access are teams in the `Mudcrab-Team` organization: `triage`, `maintainers` and `core`. Contributors have no access and no team. The Owner hat is the `owners` team, which holds Admin and the ruleset bypass. Changing someone's rank means changing their team membership. The team settings, not a list in a file, are the source of truth for who holds a rank, so no document needs updating by hand.

## As the project grows

When an area has a clear expert, such as the converter or the renderer, add a path-scoped line to CODEOWNERS (for example `/converter/ @Mudcrab-Team/converter-owners`) and keep `* @Mudcrab-Team/core` as the fallback. Add extra approval requirements only for changes that are controversial, not for everything.
