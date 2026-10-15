# Phase 2 Acceptance

The acceptance stage converts integration and profiling evidence into a reproducible release
verdict. It never copies Skyrim data into the repository or CI artifacts. Real-world acceptance
requires a legally owned, converted asset set on the target Windows GPU machine.

## Verdicts

- `accepted`: every required gate passed, a baseline was available, all real scenarios ran and the
  visual review was signed.
- `accepted-with-warnings`: automated gates passed, but non-required evidence is pending or the
  campaign was synthetic-only, baseline-free, skipped a gate, or used a dirty worktree.
- `rejected`: preflight, quality, robustness, functional, threshold, regression, screenshot, or a
  required visual gate failed.

Warnings are never silently converted into approval. `rejected` exits non-zero.
Any non-quick campaign also exits non-zero unless its verdict is exactly `accepted`; only `-Quick`
may succeed as an explicitly synthetic plumbing check with warnings.

## Campaign

Run a short asset-independent plumbing check:

```powershell
./scripts/phase2-acceptance.ps1 -Quick -SkipQualityGates -SkipRobustness -SkipBuild
```

Run the complete target-hardware campaign:

```powershell
./scripts/phase2-acceptance.ps1 `
  -Assets D:\SkyrimConverted `
  -Worldspace 0x3c `
  -RuralGridX 0 -RuralGridY 0 `
  -DenseGridX 3 -DenseGridY -12 `
  -WaterGridX 7 -WaterGridY -2 `
  -Repetitions 3 `
  -RequireVisualSignoff `
  -VisualReview D:\Evidence\visual-review.json
```

The standard places (Tamriel, `0x3c`), with the references within two cells of each (measured from
the converted world database):

| Scene | Grid | References | Notes |
| --- | --- | --- | --- |
| `rural` | 0,0 | about 1,050 | the walk, and the centre of the `stress` flight |
| `dense` | 3,-12 | about 2,900 | Riverwood, the densest cell neighbourhood in Tamriel |
| `water` | 7,-2 | about 1,580 | |
| `dense-legacy` | 18,-5 | about 1,530 | the old `dense` place; kept for one release, then removed |

`dense-legacy` is accepted by the scripts but is not in the default scene sets. Baselines and
`profiling-baseline.json` results made before this change are not comparable for `dense`, which is
now a different, about twice as dense place. The scripts do not map an old `dense` entry to
`dense-legacy`: compare an old `dense` median with a new `dense-legacy` median by hand.

Pass `-IncludeDenseLegacy` to also run `dense-legacy`.

The example passes water's coordinates explicitly: the scenario's own default is grid 0,0, the place
the rural scenario already measures, so a campaign that does not pass `-WaterGridX 7 -WaterGridY -2`
does not run the `water` place above.

Defaults are 2 minutes synthetic, 5 minutes per representative world area, 10 minutes streaming
stress and 30 minutes stability. Each scenario is run three times and evaluated by its median.

## Automated gates

Preflight records the OS, PowerShell, Cargo, Git, disk space, commit/worktree, CPU/GPU/driver,
distinct representative coordinates and exact converter/database contracts. It runs the reachable
asset closure plus a read-only audit of every published GLB and KTX2. Quality runs formatting, all
workspace tests/targets, Clippy with warnings denied and a release workspace build. Robustness
executes stale/truncated manifest, cache and database cases, deterministic worker shutdown,
bundle-output and missing-assets rejection paths.

The scenario matrix covers synthetic 250k instances, rural, dense, water, fast streaming stress and
stability. Required thresholds default to average FPS >= 60, frame P95 <= 16.67 ms, memory growth <=
0.5 GiB and zero streaming failures. Against `acceptance-baseline.json`, a material regression above
5% is a warning and above 10% is a failure. `-UpdateBaseline` stores each scenario's `place` (its
worldspace and grid, or `synthetic`) beside its medians, and a stored entry with no place or one
measured in another place — `dense` moved from 18,-5 to Riverwood — is skipped with a warning naming
the scenario and the reason rather than scored, so the campaign is never failed over a comparison
whose two runs measured different scenes; re-run once with `-UpdateBaseline` to record places. To
keep an identical build from failing on scheduler and sampling jitter, the comparison first applies
fixed absolute noise floors: 5 FPS, 1.5 ms for frame latency and 0.05 GiB for memory. The report
retains both the raw percentage and absolute delta.
The runner and its child engine use Windows `AboveNormal` process priority so unrelated desktop
work cannot preempt enough frames to create a false regression; real-time priority is never used.

## Visual evidence

The engine captures a PNG after warm-up for the first repetition of each scenario. `screenshots.json`
records its path, byte size and SHA-256. The campaign also writes `visual-review-template.json`.
Complete it with a named reviewer, signature and `pass` for every captured scenario:

```json
{
  "format_version": 2,
  "reviewer": "Reviewer Name",
  "reviewed_at": "2026-08-17T15:00:00-03:00",
  "signature": "Reviewer Name — approved",
  "checkpoints": [
    { "scenario": "materials", "status": "pass", "notes": "Canonical materials are correct." },
    { "scenario": "terrain-water", "status": "pass", "notes": "Terrain and water are coherent." },
    { "scenario": "transform-bounds", "status": "pass", "notes": "Hierarchy and bounds are correct." },
    { "scenario": "renderer", "status": "pass", "notes": "HZB and indirect rendering are stable." },
    { "scenario": "streaming", "status": "pass", "notes": "No lifecycle artifacts." },
    { "scenario": "synthetic", "status": "pass", "notes": "Synthetic scene is stable." },
    { "scenario": "rural", "status": "pass", "notes": "No terrain seams." },
    { "scenario": "dense", "status": "pass", "notes": "Materials and visibility are stable." },
    { "scenario": "water", "status": "pass", "notes": "Reflection is stable and non-recursive." },
    { "scenario": "stress", "status": "pass", "notes": "No duplicate or orphaned cells." },
    { "scenario": "stability", "status": "pass", "notes": "Memory and lifecycle remain stable." }
  ]
}
```

An empty reviewer/signature or missing checkpoint cannot satisfy visual sign-off. A final campaign
also requires three repetitions, the documented minimum durations, a same-hardware version-2
baseline and complete profiling bundles for every run.

## Evidence package

Every attempt writes `target/acceptance/<timestamp>-<hardware>/`, including metadata, preflight,
quality and robustness results, functional results, performance medians, comparisons, profiling
bundles, screenshots, visual-review template, `acceptance-report.json`, logs and
`acceptance-summary.md`. This directory is the release evidence; proprietary inputs are referenced
but never copied.

Use `-UpdateBaseline` only on a non-rejected target-hardware campaign. The manual `Phase 2
Acceptance` workflow runs on a self-hosted Windows GPU runner and retains evidence for 90 days.

## Closing Phase 2

After the comparison campaign returns exactly `accepted`, validate and close the roadmap with:

```powershell
$retainedArtifactUrl = Read-Host "Retained build artifact URL"
./scripts/phase2-close.ps1 `
  -Campaign D:\Evidence\phase2-final `
  -BundleUri $retainedArtifactUrl
```

The command rejects warnings, missing gates, an unsigned visual review, incompatible baseline,
missing or modified screenshots, dirty worktrees, and campaigns from a commit other than `HEAD`.
On success it writes `release-evidence-sha256.json` into the external bundle, records only hashes
and the external bundle reference in the repository, and marks Phase 2 complete. Use
`-ValidateOnly` to verify the bundle without changing roadmap files.
