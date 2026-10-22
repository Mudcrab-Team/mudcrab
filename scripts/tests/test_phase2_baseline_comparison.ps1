#Requires -Version 7
# Fixture tests for the baseline comparison rules in scripts/phase2-baseline.ps1: coverage of a
# metric across repeats, the samples/runs shown for a spread, and the data set identity check.
#
# The functions under test are extracted from the scripts with the PowerShell parser and run here,
# so no engine, GPU, converted data or campaign is needed. Run from the repository root:
#   pwsh -NoProfile -File scripts/tests/test_phase2_baseline_comparison.ps1
# The script exits non-zero when any fixture fails.
$ErrorActionPreference = "Stop"
$repository = Split-Path -Parent (Split-Path -Parent $PSScriptRoot)

# Defines the named functions from a script in this scope, taken from the parser's own extent of
# each definition. A missing or duplicated name is an error, so a rename cannot pass silently.
function Import-ScriptFunctions([string]$Path, [string[]]$Names) {
    $tokens = $null
    $errors = $null
    $ast = [System.Management.Automation.Language.Parser]::ParseFile($Path, [ref]$tokens, [ref]$errors)
    if ($errors.Count -gt 0) { throw "$Path does not parse: $($errors -join '; ')" }
    $definitions = @($ast.FindAll({ param($node) $node -is [System.Management.Automation.Language.FunctionDefinitionAst] }, $true))
    return @(foreach ($name in $Names) {
        $found = @($definitions | Where-Object { $_.Name -eq $name })
        if ($found.Count -ne 1) { throw "$Path defines $($found.Count) functions named $name; expected one" }
        $found[0].Extent.Text
    })
}

$baselineScript = Join-Path $repository "scripts/phase2-baseline.ps1"
$profileScript = Join-Path $repository "scripts/phase2-profile.ps1"
foreach ($text in @(Import-ScriptFunctions $profileScript @("Get-Median", "Get-RegressionComparison", "Get-ScenarioArguments", "Get-ArgumentValue"))) {
    . ([scriptblock]::Create($text))
}
$baselineFunctions = @(
    "Get-Values", "Get-Spread", "Get-SummaryMap", "Get-CountMap", "Get-SummaryLine", "Format-Spread", "Get-FieldDifferences", "Format-FieldValue",
    "Get-LaggingPasses", "Assert-AssetIdentityKnown", "Assert-ComparisonAssetIdentities",
    "Assert-WindowComparable", "Get-ComparisonRefusal", "Get-Comparison",
    "Get-RouteShortfall", "Get-BaselineProblem", "Get-RouteTravelSeconds", "Get-WindowDuration", "Get-WindowReport",
    "Get-WindowArguments", "Get-WindowInvocationParameters", "Get-WindowKinds", "Get-KnownScenarioInputs",
    "Get-PlannedWorkloads", "Assert-ComparisonWorkloads"
)
# The constants the imported functions read, taken from the script's own assignments so a changed
# value cannot leave the fixtures testing a different workload.
$baselineText = Get-Content -LiteralPath $baselineScript -Raw
function Get-ScriptConstant([string]$Name) {
    $match = [regex]::Match($baselineText, '(?m)^\$(?:script:)?' + [regex]::Escape($Name) + ' = (\d+)')
    if (-not $match.Success) { throw "$baselineScript does not assign `$$Name" }
    return [int]$match.Groups[1].Value
}
$script:baselineFormatVersion = Get-ScriptConstant "baselineFormatVersion"
$RouteReadinessSeconds = Get-ScriptConstant "RouteReadinessSeconds"
$DurationSeconds = 120
$StabilitySeconds = 1800
$routeScenarios = @{ travel = "travel.json"; check = "check.json"; long = "long.json"; loop = "loop.json" }
# Route files for the window fixtures: a long route, a looped one, and the shipped travel route.
$fixtureDirectory = Join-Path ([IO.Path]::GetTempPath()) "phase2-route-fixtures-$PID"
New-Item -ItemType Directory -Path $fixtureDirectory -Force | Out-Null
Set-Content -LiteralPath (Join-Path $fixtureDirectory "long.json") -Value '{"speed": 100, "loop": false, "waypoints": [[0, 0], [10000, 0]]}'
Set-Content -LiteralPath (Join-Path $fixtureDirectory "loop.json") -Value '{"speed": 100, "loop": true, "waypoints": [[0, 0], [10000, 0], [0, 0]]}'
# The campaign's workload variables, as the script sets them, for the planner fixtures.
$resolvedAssets = "D:\Mudcrab\Converted"
$Worldspace = "0x3c"
$RuralGridX = 0; $RuralGridY = 0; $DenseGridX = 3; $DenseGridY = -12
$DenseLegacyGridX = 18; $DenseLegacyGridY = -5; $WaterGridX = 0; $WaterGridY = 0
$StabilitySeconds = 1800
$FrameBudgetMs = 6.25
$script:knownInputKeys = Invoke-Expression ([regex]::Match($baselineText, '(?s)\$script:knownInputKeys = (@\(.*?\))').Groups[1].Value)
function Get-RouteFilePath([string]$Name) {
    if ($Name -in @("long", "loop")) { return Join-Path $fixtureDirectory $routeScenarios[$Name] }
    return Join-Path $repository "scripts/routes/$($routeScenarios[$Name])"
}
foreach ($text in @(Import-ScriptFunctions $baselineScript $baselineFunctions)) {
    . ([scriptblock]::Create($text))
}

$script:passed = 0
$script:failures = [System.Collections.Generic.List[string]]::new()

function Assert-True([bool]$Condition, [string]$Name, [string]$Detail = "") {
    if ($Condition) { $script:passed++; return }
    $script:failures.Add("${Name}: $Detail")
}

function Assert-Throws([scriptblock]$Block, [string]$Pattern, [string]$Name) {
    try {
        & $Block
    } catch {
        Assert-True ($_.Exception.Message -match $Pattern) $Name "message did not match '$Pattern': $($_.Exception.Message)"
        return
    }
    Assert-True $false $Name "expected a refusal matching '$Pattern', but the check passed"
}

function Assert-Passes([scriptblock]$Block, [string]$Name) {
    try {
        & $Block
    } catch {
        Assert-True $false $Name "expected no refusal, got: $($_.Exception.Message)"
        return
    }
    Assert-True $true $Name
}

# A baseline-shaped report with one standing-still window. `$Metrics` maps a metric name to its
# per-repeat values, `$null` where that repeat did not measure it.
function New-Report([int]$Repeats, [hashtable]$Metrics) {
    $window = [ordered]@{
        average_fps = Get-Spread $Metrics.average_fps
        p95_ms = Get-Spread $Metrics.p95
        p99_ms = Get-Spread $Metrics.p99
        memory = [ordered]@{
            process_peak_gib = Get-Spread $Metrics.peak
            process_growth_gib = Get-Spread $Metrics.growth
        }
    }
    return [ordered]@{
        repeats = $Repeats
        scenarios = @{ rural = [ordered]@{ windows = [ordered]@{ standing_still = $window; streaming = $null } } }
    }
}

# Both sides fully measured, unless a test says otherwise.
$full = @{
    average_fps = @(60.0, 60.0, 60.0); p95 = @(10.0, 10.0, 10.0); p99 = @(12.0, 12.0, 12.0)
    peak = @(2.0, 2.0, 2.0); growth = @(0.1, 0.1, 0.1)
}
# Comparison metrics retain their existing scope-qualified keys; unscored entries use metric names.
$scope = "rural/standing_still"
$metric = "frame_ms_p95"
$metricKey = "$scope/$metric"

# --- Coverage of a metric across repeats -----------------------------------------------------------

$newPartial = @{ average_fps = @(60.0, 60.0, 60.0); p95 = @(5.0, $null, $null); p99 = @(12.0, 12.0, 12.0); peak = @(2.0, 2.0, 2.0); growth = @(0.1, 0.1, 0.1) }
$comparison = Get-Comparison -Old (New-Report 3 $full) -New (New-Report 3 $newPartial)
Assert-True (-not $comparison.metrics[$scope].Contains($metricKey)) "incomplete current repeats are not scored" "scored: $($comparison.metrics[$scope][$metricKey] | ConvertTo-Json -Compress)"
Assert-True ($comparison.unscored[$scope].Contains($metric)) "incomplete current repeats are listed as unscored"
Assert-True ($comparison.unscored[$scope][$metric].reason -match "1 of 3 current repeats") "the unscored reason states the current coverage" $comparison.unscored[$scope][$metric].reason
Assert-True ($comparison.metrics[$scope].Contains("$scope/frame_ms_p99")) "a fully measured metric beside it is still scored"

$oldPartial = @{ average_fps = @(60.0, 60.0, 60.0); p95 = @(10.0, 10.0, $null); p99 = @(12.0, 12.0, 12.0); peak = @(2.0, 2.0, 2.0); growth = @(0.1, 0.1, 0.1) }
$comparison = Get-Comparison -Old (New-Report 3 $oldPartial) -New (New-Report 3 $full)
Assert-True (-not $comparison.metrics[$scope].Contains($metricKey)) "incomplete baseline repeats are not scored"
Assert-True ($comparison.unscored[$scope][$metric].reason -match "2 of 3 baseline repeats") "the unscored reason states the baseline coverage" $comparison.unscored[$scope][$metric].reason

# A one-repeat campaign measures every metric in its only repeat, so it is complete and scored.
$oneRepeatOld = New-Report 1 @{ average_fps = @(60.0); p95 = @(10.0); p99 = @(12.0); peak = @(2.0); growth = @(0.1) }
$oneRepeatNew = New-Report 1 @{ average_fps = @(60.0); p95 = @(10.5); p99 = @(12.0); peak = @(2.0); growth = @(0.1) }
$comparison = Get-Comparison -Old $oneRepeatOld -New $oneRepeatNew
Assert-True ($comparison.metrics[$scope].Contains($metricKey)) "a complete one-repeat metric is scored"
Assert-True (-not $comparison.unscored.Contains($scope)) "a complete one-repeat campaign has nothing unscored"

# A metric no repeat measured is omitted from both maps: no fabricated zero, no fabricated verdict.
$allNull = @{ average_fps = @(60.0, 60.0, 60.0); p95 = @($null, $null, $null); p99 = @(12.0, 12.0, 12.0); peak = @(2.0, 2.0, 2.0); growth = @(0.1, 0.1, 0.1) }
$comparison = Get-Comparison -Old (New-Report 3 $full) -New (New-Report 3 $allNull)
Assert-True (-not $comparison.metrics[$scope].Contains($metricKey)) "an all-null metric is not scored"
Assert-True (-not ($comparison.unscored.Contains($scope) -and $comparison.unscored[$scope].Contains($metric))) "an all-null metric is not listed as unscored either"

# Markdown shows the coverage of a partial spread next to its range.
$partialSpread = Get-Spread @(8.0, $null, $null)
Assert-True ((Format-Spread $partialSpread) -match "1/3 samples") "a partial spread shows samples/runs" (Format-Spread $partialSpread)
$completeSpread = Get-Spread @(8.0, 9.0, 10.0)
Assert-True ((Format-Spread $completeSpread) -match "3/3 samples") "a complete spread shows samples/runs" (Format-Spread $completeSpread)
Assert-True ((Format-Spread $null) -eq "n/a") "a metric no repeat measured reads n/a"

# Exercise the report aggregation path, including absent GPU-pass keys, before formatting.
$gpuReports = @(
    @{ passes = @{ opaque = @{ mean = 8.0; count = 20 } } },
    @{ passes = @{} },
    @{ passes = @{} }
)
$gpuPasses = Get-SummaryMap $gpuReports { param($report) $report.passes }
Assert-True ($gpuPasses.opaque.samples -eq 1 -and $gpuPasses.opaque.runs -eq 3) "GPU pass aggregation retains missing-repeat coverage"
Assert-True ((Get-SummaryLine $gpuPasses) -match "1/3 samples") "GPU pass summary shows actual repeat coverage" (Get-SummaryLine $gpuPasses)
$lagging = Get-LaggingPasses @{
    gpu_passes = @{
        measured_frames = Get-Spread @(100, 100, 100)
        passes = $gpuPasses
        pass_samples = Get-CountMap $gpuReports { param($report) $report.passes }
    }
}
Assert-True (($lagging.passes -join "; ") -match "1/3 samples") "lagging GPU pass list shows repeat coverage" ($lagging.passes -join "; ")

# --- Data set identity -------------------------------------------------------------------------------

function New-AssetWindow([string]$Assets, $Identity, [string]$Unavailable, [string]$Hash = "h1") {
    $inputs = [ordered]@{
        worldspace = "Tamriel"; assets = $Assets; fly_speed = $null; synthetic_instances = $null
        assets_identity = if ($null -eq $Identity) { [ordered]@{ world_database_bytes = $null; world_database_modified_utc = $null; cell_cache_bytes = $null; configuration_hash = $null; schema_version = $null } } else { $Identity }
        assets_identity_unavailable = $Unavailable
        stream_radius = 3; duration_seconds = 120; warmup_frames = 60
    }
    return [ordered]@{
        scenario_inputs = $inputs
        fingerprint = [ordered]@{ graphics = [ordered]@{ stream_radius = 3 }; opened_world_database_schema = 9 }
    }
}
function New-Identity([string]$Hash) {
    return [ordered]@{ world_database_bytes = 100; world_database_modified_utc = "2026-10-01T00:00:00.0000000Z"; cell_cache_bytes = 50; configuration_hash = $Hash; schema_version = 9 }
}
$unknownReason = "conversion-manifest.json records no configuration_hash for the data set"

# Both windows name one data set with no known identity and the same unavailable reason: refused.
$sameUnknown = New-AssetWindow "D:\Mudcrab\Converted" (New-Identity "") $unknownReason
Assert-Throws { Assert-WindowComparable $sameUnknown $sameUnknown "baseline.json" "rural/standing_still" } "no known identity" "two unknown identities for the same path are refused"

# One side unknown, the other known: refused, the reason names the unknown side's data set.
$known = New-AssetWindow "D:\Mudcrab\Converted" (New-Identity "h1") $null
$unknownSide = New-AssetWindow "D:\Mudcrab\Converted" (New-Identity "") $unknownReason
Assert-Throws { Assert-WindowComparable $known $unknownSide "baseline.json" "rural/standing_still" } "no known identity" "an unknown identity on the current side is refused"
Assert-Throws { Assert-WindowComparable $unknownSide $known "baseline.json" "rural/standing_still" } "no known identity" "an unknown identity on the baseline side is refused"

# The synthetic scene loads no data set, so its unavailable reason is not an identity problem.
$synthetic = New-AssetWindow $null $null "this window was started without -Assets, so it loaded no converted data set"
Assert-Passes { Assert-WindowComparable $synthetic $synthetic "baseline.json" "synthetic/standing_still" } "synthetic windows without assets still compare"

# Known and matching identities compare; a different configuration hash is a different workload.
Assert-Passes { Assert-WindowComparable $known (New-AssetWindow "D:\Mudcrab\Converted" (New-Identity "h1") $null) "baseline.json" "rural/standing_still" } "matching known identities compare"
$differentHash = New-AssetWindow "D:\Mudcrab\Converted" (New-Identity "h2") $null
Assert-Throws { Assert-WindowComparable $known $differentHash "baseline.json" "rural/standing_still" } "not the same workload" "differing known identities are refused"

# -CompareTo refuses by throwing; a baseline picked up from -Out warns and is dropped instead.
$explicitCompare = $true
Assert-Throws { Get-ComparisonRefusal { Assert-WindowComparable $sameUnknown $sameUnknown "baseline.json" "rural/standing_still" } } "no known identity" "an explicit comparison throws on an unknown identity"
$explicitCompare = $false
$refusal = Get-ComparisonRefusal { Assert-WindowComparable $sameUnknown $sameUnknown "baseline.json" "rural/standing_still" }
Assert-True ($refusal -match "no known identity") "an implicit comparison returns the refusal for a warning" $refusal
Assert-True ($null -eq (Get-ComparisonRefusal { Assert-WindowComparable $known $known "baseline.json" "rural/standing_still" })) "an implicit comparison that passes returns no refusal"

# Preflight uses a controlled identity provider and never invokes the engine or a campaign.
function Get-AssetsIdentity([string]$ResolvedAssets) { return $script:preflightIdentity }
$script:preflightIdentity = @{ unavailable = $unknownReason }
$oldKnown = @{ scenarios = @{ rural = @{ windows = @{ standing_still = $known } } } }
$oldUnknown = @{ scenarios = @{ rural = @{ windows = @{ standing_still = $unknownSide } } } }
$windowCounts = @{ rural = 1; synthetic = 1 }
$explicitCompare = $true
Assert-Throws { Get-ComparisonRefusal { Assert-ComparisonAssetIdentities $oldKnown "D:\Mudcrab\Converted" @("synthetic", "rural") $windowCounts "baseline.json" } } "no known identity" "preflight refuses a current unknown identity before timed windows"
$script:preflightIdentity = @{ unavailable = $null }
Assert-Throws { Assert-ComparisonAssetIdentities $oldUnknown "D:\Mudcrab\Converted" @("rural") $windowCounts "baseline.json" } "no known identity" "preflight refuses an unknown baseline identity"
Assert-Passes { Assert-ComparisonAssetIdentities $oldKnown "D:\Mudcrab\Converted" @("rural") $windowCounts "baseline.json" } "preflight accepts known identities"
$script:preflightIdentity = @{ unavailable = $unknownReason }
Assert-Passes { Assert-ComparisonAssetIdentities $oldUnknown $null @("synthetic") $windowCounts "baseline.json" } "synthetic-only preflight needs no asset identity"
$explicitCompare = $false
$refusal = Get-ComparisonRefusal { Assert-ComparisonAssetIdentities $oldKnown "D:\Mudcrab\Converted" @("rural") $windowCounts "baseline.json" }
Assert-True ($refusal -match "no known identity") "implicit preflight returns a warning reason instead of throwing" $refusal

$tokens = $null; $errors = $null
$ast = [System.Management.Automation.Language.Parser]::ParseFile($baselineScript, [ref]$tokens, [ref]$errors)
$preflightCalls = @($ast.FindAll({ param($node) $node -is [System.Management.Automation.Language.CommandAst] -and $node.GetCommandName() -eq "Assert-ComparisonAssetIdentities" }, $true))
$engineCalls = @($ast.FindAll({ param($node) $node -is [System.Management.Automation.Language.CommandAst] -and $node.GetCommandName() -eq "Invoke-EngineWindow" }, $true))
Assert-True ($preflightCalls.Count -eq 1 -and $engineCalls.Count -eq 1 -and $preflightCalls[0].Extent.StartOffset -lt $engineCalls[0].Extent.StartOffset) "campaign identity preflight precedes its timed engine invocation"

# --- Route windows: partial routes are never scored or compared as full ----------------------------

function New-RouteWindow([int]$Runs, [int]$Completed, [int]$Started, [string]$File = "travel.json") {
    return [ordered]@{ window = "streaming"; runs = $Runs; route = [ordered]@{ file = $File; runs_completed = $Completed; runs_started = $Started } }
}

Assert-True ($null -eq (Get-RouteShortfall (New-RouteWindow 3 3 3))) "a route window whose every run completed has no shortfall"
Assert-True ((Get-RouteShortfall (New-RouteWindow 3 2 3)) -match "2 of 3 runs completed") "a route window with a run that stopped short names the shortfall" (Get-RouteShortfall (New-RouteWindow 3 2 3))
Assert-True ((Get-RouteShortfall (New-RouteWindow 3 0 0)) -match "0 of 3 runs completed") "a route whose runs never started names the shortfall"
Assert-True ($null -eq (Get-RouteShortfall ([ordered]@{ window = "standing_still"; runs = 3; route = $null }))) "a window with no route has no route shortfall"

$partialBaseline = @{
    format_version = $script:baselineFormatVersion
    fingerprint = @{}
    scenarios = @{ travel = @{ windows = @{ streaming = (New-RouteWindow 3 2 3) } } }
}
$completeBaseline = @{
    format_version = $script:baselineFormatVersion
    fingerprint = @{}
    scenarios = @{ travel = @{ windows = @{ streaming = (New-RouteWindow 3 3 3) } } }
}
Assert-True ((Get-BaselineProblem $partialBaseline) -match "partial route window") "a baseline holding a partial route is refused before the campaign" (Get-BaselineProblem $partialBaseline)
Assert-True ($null -eq (Get-BaselineProblem $completeBaseline)) "a baseline whose route windows all completed is accepted"
Assert-Throws { Assert-WindowComparable (New-RouteWindow 3 2 3) (New-RouteWindow 3 3 3) "baseline.json" "travel/streaming" } "partial route window" "an explicit comparison refuses a partial baseline route window"
Assert-Throws { Assert-WindowComparable (New-RouteWindow 3 3 3) (New-RouteWindow 3 0 0) "baseline.json" "travel/streaming" } "partial route window" "an explicit comparison refuses a current route that never started"

# The window's time limit is a floor; a route window runs at least its travel time plus the readiness allowance.
Assert-True ((Get-RouteTravelSeconds "travel") -eq 60) "the shipped travel route takes 60 s at its speed" "$(Get-RouteTravelSeconds 'travel')"
Assert-True ((Get-WindowDuration "travel") -eq 120) "travel's window keeps the 120 s limit when travel plus 60 s is shorter" "$(Get-WindowDuration 'travel')"
Assert-True ((Get-RouteTravelSeconds "long") -eq 100) "a 10000-unit route at 100 units/s takes 100 s"
Assert-True ((Get-WindowDuration "long") -eq 160) "a route window adds the 60 s readiness allowance to its travel time" "$(Get-WindowDuration 'long')"
$DurationSeconds = 300
Assert-True ((Get-WindowDuration "travel") -eq 300) "a longer time limit is kept as the route window's minimum"
$DurationSeconds = 120
Assert-Throws { Get-RouteTravelSeconds "loop" } "loops" "a looped route has no travel time to allow for"

# The scoring gate: the baseline's own window reader must check route completion before it scores.
$windowReportAst = $null
$tokens = $null; $errors = $null
$windowReportAst = [System.Management.Automation.Language.Parser]::ParseFile($baselineScript, [ref]$tokens, [ref]$errors).FindAll({ param($node) $node -is [System.Management.Automation.Language.FunctionDefinitionAst] -and $node.Name -eq "Get-WindowReport" }, $true)[0]
$shortfallCalls = @($windowReportAst.FindAll({ param($node) $node -is [System.Management.Automation.Language.CommandAst] -and $node.GetCommandName() -eq "Get-RouteShortfall" }, $true))
Assert-True ($shortfallCalls.Count -eq 1) "Get-WindowReport refuses a partial route before it builds the window's numbers"

Remove-Item -LiteralPath $fixtureDirectory -Recurse -Force

# --- Planned workloads: refused before the first window, whichever input changed --------------------

$script:preflightIdentity = @{ unavailable = $null; configuration_hash = "h1"; schema_version = 1 }
$planCounts = @{ rural = 1; dense = 1; long = 1; synthetic = 1 }
$requestedPlan = @("rural", "dense", "long")
# A baseline shaped as the campaign writes one: each window's scenario inputs are the planned ones.
function New-PlannedBaseline($Planned, [double]$BudgetMs = 6.25) {
    $windows = @{}
    foreach ($key in @($Planned.Keys)) {
        $parts = $key -split "/"
        if (-not $windows.ContainsKey($parts[0])) { $windows[$parts[0]] = @{ windows = @{} } }
        $windows[$parts[0]].windows[$parts[1]] = @{ scenario_inputs = $Planned[$key]; frame_budget = @{ budget_ms = $BudgetMs } }
    }
    return @{ scenarios = $windows }
}

New-Item -ItemType Directory -Path $fixtureDirectory -Force | Out-Null
Set-Content -LiteralPath (Join-Path $fixtureDirectory "long.json") -Value '{"speed": 100, "loop": false, "waypoints": [[0, 0], [10000, 0]]}'
$planned = Get-PlannedWorkloads $requestedPlan $planCounts
Assert-True ($planned.Keys -contains "long/streaming" -and -not ($planned.Keys -contains "long/standing_still")) "a route scenario plans its streaming window only" ($planned.Keys -join ",")
Assert-True ($planned["dense/standing_still"].grid.x -eq 3 -and $planned["dense/standing_still"].duration_seconds -eq 120) "the planner takes the dense grid and the window duration from the campaign's arguments"
Assert-True ($planned["long/streaming"].route.sha256.Length -gt 0) "a planned route window carries its file's hash"
Assert-True (-not $planned["rural/standing_still"].Contains("stream_radius")) "the planner keeps report-only fields out of the pre-run check"

$unchangedBaseline = New-PlannedBaseline $planned
Assert-Passes { Assert-ComparisonWorkloads $unchangedBaseline $planned $FrameBudgetMs "baseline.json" } "an identical plan passes before the first window"

# A changed route file is a different workload, whatever the other windows say.
Set-Content -LiteralPath (Join-Path $fixtureDirectory "long.json") -Value '{"speed": 100, "loop": false, "waypoints": [[0, 0], [9000, 0]]}'
$editedRoute = Get-PlannedWorkloads $requestedPlan $planCounts
Assert-Throws { Assert-ComparisonWorkloads $unchangedBaseline $editedRoute $FrameBudgetMs "baseline.json" } "route" "a changed route file's coordinates are refused before any window"
Set-Content -LiteralPath (Join-Path $fixtureDirectory "long.json") -Value '{"speed": 100, "loop": false, "waypoints": [[0, 0], [10000, 0]]}'

# A later scenario's changed grid is refused even though the first scenario still matches.
$DenseGridX = 4
$movedDense = Get-PlannedWorkloads $requestedPlan $planCounts
$DenseGridX = 3
Assert-Throws { Assert-ComparisonWorkloads $unchangedBaseline $movedDense $FrameBudgetMs "baseline.json" } "grid" "a later scenario whose grid moved is refused before the first window"
Assert-True ($movedDense["rural/standing_still"].grid.x -eq $planned["rural/standing_still"].grid.x) "the earlier scenario's plan is unchanged by a later scenario's grid"

# A changed time limit or frame budget is a different workload too.
$DurationSeconds = 200
$longerPlan = Get-PlannedWorkloads $requestedPlan $planCounts
$DurationSeconds = 120
Assert-Throws { Assert-ComparisonWorkloads $unchangedBaseline $longerPlan $FrameBudgetMs "baseline.json" } "duration_seconds" "a changed time limit is refused before the first window"
Assert-Throws { Assert-ComparisonWorkloads $unchangedBaseline $planned 8.33 "baseline.json" } "frame_budget_ms" "a changed frame budget is refused before the first window"

# Explicit comparisons throw; an implicit one (a baseline from -Out) returns the reason to warn about.
$explicitCompare = $true
Assert-Throws { Get-ComparisonRefusal { Assert-ComparisonWorkloads $unchangedBaseline $movedDense $FrameBudgetMs "baseline.json" } } "grid" "an explicit planned-workload mismatch throws"
$explicitCompare = $false
$refusal = Get-ComparisonRefusal { Assert-ComparisonWorkloads $unchangedBaseline $movedDense $FrameBudgetMs "baseline.json" }
Assert-True ($refusal -match "grid") "an implicit planned-workload mismatch returns the reason for a warning" $refusal

# The route kind is streaming, and the check runs before the first engine window in the script.
$scriptAst = [System.Management.Automation.Language.Parser]::ParseFile($baselineScript, [ref]$null, [ref]$null)
$checkCalls = @($scriptAst.FindAll({ param($node) $node -is [System.Management.Automation.Language.CommandAst] -and $node.GetCommandName() -eq "Assert-ComparisonWorkloads" }, $true))
$windowCalls = @($scriptAst.FindAll({ param($node) $node -is [System.Management.Automation.Language.CommandAst] -and $node.GetCommandName() -eq "Invoke-EngineWindow" }, $true))
Assert-True ($checkCalls.Count -eq 1 -and $windowCalls.Count -ge 1 -and $checkCalls[0].Extent.StartOffset -lt $windowCalls[0].Extent.StartOffset) "the planned-workload check precedes the first Invoke-EngineWindow" "check=$($checkCalls.Count) windows=$($windowCalls.Count)"
Assert-True ((Get-WindowKinds "long" $planCounts) -join "," -eq "streaming") "a route scenario's window kind is streaming"

Remove-Item -LiteralPath $fixtureDirectory -Recurse -Force

# --- Result ------------------------------------------------------------------------------------------

Write-Host "phase2 baseline comparison fixtures: $script:passed passed, $($script:failures.Count) failed"
foreach ($failure in $script:failures) { Write-Host "FAILED $failure" }
if ($script:failures.Count -gt 0) { exit 1 }
