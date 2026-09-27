[CmdletBinding()]
param(
    [Parameter(Mandatory = $true)]
    [string]$Campaign,
    [string]$BundleUri,
    [switch]$ValidateOnly
)

$ErrorActionPreference = "Stop"
$repository = Split-Path -Parent $PSScriptRoot

function Assert-Phase2Condition([bool]$Condition, [string]$Message) {
    if (-not $Condition) { throw "Phase 2 cannot be closed: $Message" }
}

function Write-Utf8File([string]$Path, [string]$Content) {
    [IO.File]::WriteAllText($Path, $Content, [Text.UTF8Encoding]::new($false))
}

$campaignPath = (Resolve-Path -LiteralPath $Campaign).Path
$reportPath = Join-Path $campaignPath "acceptance-report.json"
Assert-Phase2Condition (Test-Path -LiteralPath $reportPath -PathType Leaf) "acceptance-report.json is missing"

$report = Get-Content -LiteralPath $reportPath -Raw | ConvertFrom-Json
Assert-Phase2Condition ($report.format_version -eq 2) "acceptance report format must be 2"
Assert-Phase2Condition ($report.verdict -eq "accepted") "verdict is '$($report.verdict)', expected 'accepted'"
Assert-Phase2Condition ([bool]$report.release_ready) "release_ready is false"
Assert-Phase2Condition (@($report.failures).Count -eq 0) "acceptance report contains failures"
Assert-Phase2Condition (@($report.warnings).Count -eq 0) "acceptance report contains warnings"
Assert-Phase2Condition ([bool]$report.preflight_passed) "preflight did not pass"
Assert-Phase2Condition ([bool]$report.quality_passed) "quality gates did not pass"
Assert-Phase2Condition ([bool]$report.robustness_passed) "robustness gates did not pass"
Assert-Phase2Condition ([bool]$report.release_build_passed) "release build did not pass"
Assert-Phase2Condition ([bool]$report.functional_passed) "functional gates did not pass"
Assert-Phase2Condition ([bool]$report.baseline.available) "acceptance baseline is missing"
Assert-Phase2Condition ([bool]$report.baseline.same_hardware) "acceptance baseline is from different hardware"
Assert-Phase2Condition ([bool]$report.visual.passed) "signed visual review did not pass"
Assert-Phase2Condition (-not [bool]$report.metadata.dirty_worktree) "campaign used a dirty worktree"

$expectedScenarios = @($report.expected_scenarios)
$actualScenarios = @($report.scenarios.PSObject.Properties.Name)
Assert-Phase2Condition ($expectedScenarios.Count -eq 11) "all 11 Phase 2 scenarios are required"
foreach ($scenario in $expectedScenarios) {
    Assert-Phase2Condition ($scenario -in $actualScenarios) "scenario '$scenario' is missing"
}

$currentCommit = (& git -C $repository rev-parse --short=12 HEAD 2>$null)
Assert-Phase2Condition ([bool]$currentCommit) "current Git commit could not be read"
Assert-Phase2Condition ($report.metadata.commit -eq $currentCommit) "campaign commit '$($report.metadata.commit)' does not match HEAD '$currentCommit'"
& git -C $repository diff --quiet --ignore-submodules HEAD 2>$null
Assert-Phase2Condition ($LASTEXITCODE -eq 0) "tracked worktree changes are present"

$baselinePath = $report.baseline.path
Assert-Phase2Condition ([bool]$baselinePath) "baseline path is empty"
Assert-Phase2Condition (Test-Path -LiteralPath $baselinePath -PathType Leaf) "baseline file is missing"
$baseline = Get-Content -LiteralPath $baselinePath -Raw | ConvertFrom-Json
Assert-Phase2Condition ($baseline.format_version -eq 2) "baseline format must be 2"
Assert-Phase2Condition ($baseline.hardware -eq $report.metadata.hardware) "baseline hardware does not match the campaign"
foreach ($scenario in $expectedScenarios) {
    Assert-Phase2Condition ($scenario -in @($baseline.scenarios.PSObject.Properties.Name)) "baseline scenario '$scenario' is missing"
}

$screenshotsPath = Join-Path $campaignPath "screenshots.json"
$visualReviewPath = Join-Path $campaignPath "visual-review.json"
Assert-Phase2Condition (Test-Path -LiteralPath $screenshotsPath -PathType Leaf) "screenshots.json is missing"
Assert-Phase2Condition (Test-Path -LiteralPath $visualReviewPath -PathType Leaf) "visual-review.json is missing"
$screenshots = @(Get-Content -LiteralPath $screenshotsPath -Raw | ConvertFrom-Json)
$visualReview = Get-Content -LiteralPath $visualReviewPath -Raw | ConvertFrom-Json
Assert-Phase2Condition ([bool]$visualReview.reviewer) "visual reviewer is empty"
Assert-Phase2Condition ([bool]$visualReview.reviewed_at) "visual review timestamp is empty"
Assert-Phase2Condition ([bool]$visualReview.signature) "visual review signature is empty"
foreach ($scenario in $expectedScenarios) {
    $entry = @($screenshots | Where-Object { $_.scenario -eq $scenario })
    Assert-Phase2Condition ($entry.Count -eq 1) "screenshot evidence for '$scenario' is missing or duplicated"
    Assert-Phase2Condition ([bool]$entry[0].present) "screenshot for '$scenario' was not captured"
    Assert-Phase2Condition (Test-Path -LiteralPath $entry[0].path -PathType Leaf) "screenshot file for '$scenario' is missing"
    $actualHash = (Get-FileHash -LiteralPath $entry[0].path -Algorithm SHA256).Hash
    Assert-Phase2Condition ($actualHash -eq $entry[0].sha256) "screenshot hash mismatch for '$scenario'"
    $checkpoint = @($visualReview.checkpoints | Where-Object { $_.scenario -eq $scenario -and $_.status -eq "pass" })
    Assert-Phase2Condition ($checkpoint.Count -eq 1) "visual checkpoint for '$scenario' is not passed exactly once"
}

$minimumDurations = @{
    materials = 120; "terrain-water" = 120; "transform-bounds" = 120; renderer = 120
    streaming = 120; synthetic = 120; rural = 300; dense = 300; water = 300
    stress = 600; stability = 1800
}
foreach ($scenario in $expectedScenarios) {
    for ($run = 1; $run -le 3; $run++) {
        $runReportPath = Join-Path $campaignPath "profiling\$scenario\run-$run\acceptance.json"
        Assert-Phase2Condition (Test-Path -LiteralPath $runReportPath -PathType Leaf) "profiling report for '$scenario' run $run is missing"
        $runReport = Get-Content -LiteralPath $runReportPath -Raw | ConvertFrom-Json
        Assert-Phase2Condition ([bool]$runReport.passed) "profiling report for '$scenario' run $run did not pass"
        Assert-Phase2Condition ([double]$runReport.elapsed_seconds -ge $minimumDurations[$scenario]) "profiling duration for '$scenario' run $run is below $($minimumDurations[$scenario]) seconds"
    }
}

$requiredEvidence = @(
    "acceptance-report.json",
    "acceptance-summary.md",
    "preflight.json",
    "quality-gates.json",
    "robustness.json",
    "functional-results.json",
    "comparison.json",
    "screenshots.json",
    "visual-review.json",
    "performance/medians.json"
)
$evidence = foreach ($relativePath in $requiredEvidence) {
    $path = Join-Path $campaignPath $relativePath
    Assert-Phase2Condition (Test-Path -LiteralPath $path -PathType Leaf) "evidence '$relativePath' is missing"
    $item = Get-Item -LiteralPath $path
    [ordered]@{
        path = $relativePath.Replace('\', '/')
        bytes = $item.Length
        sha256 = (Get-FileHash -LiteralPath $path -Algorithm SHA256).Hash
    }
}
$manifest = [ordered]@{
    format_version = 1
    generated_at = $report.metadata.generated_at
    campaign_commit = $report.metadata.commit
    campaign_hardware = $report.metadata.hardware
    baseline_sha256 = (Get-FileHash -LiteralPath $baselinePath -Algorithm SHA256).Hash
    files = @($evidence)
}
$manifestPath = Join-Path $campaignPath "release-evidence-sha256.json"
$manifestContent = ($manifest | ConvertTo-Json -Depth 6) + "`n"
$manifestBytes = [Text.Encoding]::UTF8.GetBytes($manifestContent)
$manifestHash = [Convert]::ToHexString([Security.Cryptography.SHA256]::HashData($manifestBytes))

if ($ValidateOnly) {
    Write-Host "Phase 2 closure validation passed: $campaignPath"
    Write-Host "Evidence manifest SHA-256: $manifestHash"
    exit 0
}

Assert-Phase2Condition ([bool]$BundleUri) "BundleUri is required when closing the roadmap"
$parsedBundleUri = $null
Assert-Phase2Condition ([Uri]::TryCreate($BundleUri, [UriKind]::Absolute, [ref]$parsedBundleUri)) "BundleUri must be an absolute URL"
Assert-Phase2Condition ($parsedBundleUri.Scheme -eq "https") "BundleUri must use HTTPS"
Write-Utf8File $manifestPath $manifestContent
$bundleReference = if ($BundleUri) { "[$BundleUri]($BundleUri)" } else { "``$campaignPath``" }
$releaseEvidencePath = Join-Path $repository "docs\roadmap\02-release-evidence.md"
$releaseEvidenceTemplate = @'
# Phase 2 release evidence

Phase 2 was closed from a reproducible target-hardware campaign with verdict exactly `accepted`.
The proprietary converted assets and campaign bundle are not stored in this repository.

- Campaign commit: `{0}`
- Hardware: {1}
- Bundle: {2}
- Evidence manifest SHA-256: `{3}`
- Baseline SHA-256: `{4}`
- Visual review: signed and passed
- Quality, robustness, release build and functional gates: passed
- Scenarios: {5}

The per-file hashes are recorded in `release-evidence-sha256.json` inside the external bundle.
'@
$releaseEvidence = $releaseEvidenceTemplate -f @(
    $report.metadata.commit,
    $report.metadata.hardware,
    $bundleReference,
    $manifestHash,
    $manifest.baseline_sha256,
    ($expectedScenarios -join ', ')
)
Write-Utf8File $releaseEvidencePath ($releaseEvidence.TrimEnd() + "`n")

$readmePath = Join-Path $repository "README.md"
$readme = Get-Content -LiteralPath $readmePath -Raw
$pendingRoadmap = '- [ ] **[Phase 2: Core Engine Runtime & Vercidium Renderer (`engine`)](docs/roadmap/02-core-engine.md)** — Runtime, integration, profiling, and acceptance infrastructure implemented; complete real-asset sign-off remains pending.'
$completeRoadmap = '- [x] **[Phase 2: Core Engine Runtime & Vercidium Renderer (`engine`)](docs/roadmap/02-core-engine.md)** — Completed with reproducible real-asset acceptance; see the [release evidence](docs/roadmap/02-release-evidence.md).'
Assert-Phase2Condition ($readme.Contains($pendingRoadmap)) "README Phase 2 pending marker was not found"
Write-Utf8File $readmePath ($readme.Replace($pendingRoadmap, $completeRoadmap))

$corePath = Join-Path $repository "docs\roadmap\02-core-engine.md"
$core = Get-Content -LiteralPath $corePath -Raw
$pendingStatus = '> **Status: Acceptance pending.** Runtime, real-asset closure, HZB/indirect-renderer conformance and automated gates are implemented. The three-repetition target-hardware campaign, approved baseline, and signed visual review remain required before completion.'
$completeStatus = '> **Status: Complete.** Runtime and target-hardware acceptance passed. See [Phase 2 release evidence](02-release-evidence.md) for the external bundle and integrity hashes.'
Assert-Phase2Condition ($core.Contains($pendingStatus)) "core-engine pending status was not found"
Write-Utf8File $corePath ($core.Replace($pendingStatus, $completeStatus))

$planPath = Join-Path $repository "docs\roadmap\02-completion-plan.md"
$plan = Get-Content -LiteralPath $planPath -Raw
$planHeading = "# Plano de conclusão da Phase 2`r`n"
if (-not $plan.Contains($planHeading)) { $planHeading = "# Plano de conclusão da Phase 2`n" }
Assert-Phase2Condition ($plan.Contains($planHeading)) "completion-plan heading was not found"
$planStatus = "$planHeading`n> **Status: Complete.** The accepted campaign and integrity hashes are recorded in [Phase 2 release evidence](02-release-evidence.md).`n"
Write-Utf8File $planPath ($plan.Replace($planHeading, $planStatus))

Write-Host "Phase 2 documentation was closed successfully."
Write-Host "Release evidence: $releaseEvidencePath"
Write-Host "Evidence manifest SHA-256: $manifestHash"
