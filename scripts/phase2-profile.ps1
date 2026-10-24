[CmdletBinding()]
param(
    [ValidateSet("all", "synthetic", "rural", "dense", "dense-legacy", "water", "stress", "stability")]
    [string]$Scenario = "all",
    [string]$Assets,
    [string]$Worldspace = "0x3c",
    [int]$RuralGridX = 0,
    [int]$RuralGridY = 0,
    # Dense is Riverwood (about 2,900 references within two cells). `dense-legacy` is the place
    # `dense` used to be (18,-5, about 1,530), kept for one release; the scripts do not map an old `dense` entry to it.
    [int]$DenseGridX = 3,
    [int]$DenseGridY = -12,
    [int]$DenseLegacyGridX = 18,
    [int]$DenseLegacyGridY = -5,
    [int]$WaterGridX = 0,
    [int]$WaterGridY = 0,
    [int]$Repetitions = 3,
    [int]$DurationSeconds = 120,
    [int]$StabilitySeconds = 1800,
    [string]$Baseline = "profiling-baseline.json",
    [switch]$UpdateBaseline,
    [switch]$Quick,
    [switch]$SkipBuild,
    # Import the functions below and stop, instead of running a campaign: phase2-baseline.ps1
    # dot-sources this file with -AsLibrary so both scripts invoke the engine the same way.
    [switch]$AsLibrary
)

$ErrorActionPreference = "Stop"
# `ConvertFrom-Json -AsHashtable` (used for report and baseline files) arrived in PowerShell 6, so
# under Windows PowerShell 5.1 the failure would surface as a puzzling "parameter not found".
if ($PSVersionTable.PSVersion.Major -lt 7) {
    throw "Phase 2 campaign scripts need PowerShell 7 (pwsh): run again as 'pwsh -File scripts/$(Split-Path -Leaf $PSCommandPath) ...'"
}
$script:repository = Split-Path -Parent $PSScriptRoot
# The report format this script reads back. The engine writes it into every benchmark report; a
# `frame-metrics.json` left by another build is not accepted as this invocation's report.
$script:benchmarkReportFormatVersion = 11

function Get-ProfilingRepository {
    return $script:repository
}

# Where the release engine binary is. `CARGO_TARGET_DIR`, when the caller set it, wins over the
# repository's own `target` directory (the worktree convention on this machine).
function Get-EngineBinaryPath {
    $targetRoot = $script:repository + "\target"
    if ($env:CARGO_TARGET_DIR) {
        $targetRoot = if ([IO.Path]::IsPathRooted($env:CARGO_TARGET_DIR)) {
            $env:CARGO_TARGET_DIR
        } else {
            Join-Path $script:repository $env:CARGO_TARGET_DIR
        }
    }
    return Join-Path $targetRoot "release\engine.exe"
}

# The hardware label a run identifies itself by: CPU, GPU and driver, reduced to one token so it
# can be passed on the command line and compared between runs of the same machine.
function Get-BenchmarkHardware {
    $cpu = (Get-CimInstance Win32_Processor | Select-Object -First 1 -ExpandProperty Name) -replace '[^a-zA-Z0-9]+', '-'
    $gpuInfo = Get-CimInstance Win32_VideoController | Select-Object -First 1
    $gpu = $gpuInfo.Name -replace '[^a-zA-Z0-9]+', '-'
    $driver = $gpuInfo.DriverVersion -replace '[^a-zA-Z0-9.]+', '-'
    return "$cpu-$gpu-driver-$driver".Trim('-')
}

# Builds the release engine once. `-Jobs 1` keeps the campaign's single-job build (the profiling
# run wants the machine quiet, not hot); the baseline command builds with cargo's default.
function Invoke-EngineReleaseBuild {
    param([int]$Jobs = 0)
    $arguments = @("build", "--release", "--bin", "engine")
    if ($Jobs -gt 0) { $arguments += "-j$Jobs" }
    Push-Location $script:repository
    try {
        # Cargo writes its progress to stderr, which PowerShell 5.1 would turn into an error record
        # under this script's `$ErrorActionPreference = "Stop"`; the preference is relaxed for the
        # call so a build cannot abort on its own progress lines.
        $previous = $ErrorActionPreference
        $ErrorActionPreference = "Continue"
        try {
            & cargo @arguments
            $exitCode = $LASTEXITCODE
        } finally {
            $ErrorActionPreference = $previous
        }
        if ($exitCode -ne 0) { throw "Release build failed with exit code $exitCode" }
    } finally {
        Pop-Location
    }
}

# What this campaign is: the commit it was built from and whether the worktree was dirty.
#
# Git writes line-ending and progress warnings to stderr, which PowerShell 5.1 turns into an error
# record under this script's `$ErrorActionPreference = "Stop"`, so the preference is relaxed around
# the two calls (a warning is not a failed command).
function Get-BenchmarkRunIdentity {
    $previous = $ErrorActionPreference
    $ErrorActionPreference = "Continue"
    try {
        $commit = (& git -C $script:repository rev-parse --short=12 HEAD 2>$null)
        if (-not $commit) { $commit = "unknown" }
        & git -C $script:repository diff --quiet --ignore-submodules HEAD 2>$null
        $dirty = $LASTEXITCODE -ne 0
    } finally {
        $ErrorActionPreference = $previous
    }
    return [ordered]@{ commit = $commit; dirty = $dirty }
}

# The converted asset set a world scenario loads, checked for the four files every one of them
# needs, or $null when no -Assets was given. A relative path is taken from the repository root, the
# directory both campaign scripts run from.
function Resolve-ConvertedAssetSet {
    param([string]$Assets)
    if (-not $Assets) { return $null }
    $candidate = if ([IO.Path]::IsPathRooted($Assets)) { $Assets } else { Join-Path $script:repository $Assets }
    $resolved = (Resolve-Path -LiteralPath $candidate).Path
    foreach ($required in @("conversion-manifest.json", "integration-report.json", "skyrim_world.db", "cell_cache.rkyv")) {
        if (-not (Test-Path -LiteralPath (Join-Path $resolved $required) -PathType Leaf)) {
            throw "Converted asset set is missing $required"
        }
    }
    return $resolved
}

# The engine's arguments for one window of one scenario: the scenario's own definition (fixture
# or world, its grid, its camera movement) and the output paths this run writes.
#
# `-StandingStill` is the same scenario with the camera left in place: it drops the
# `--auto-fly-speed` a moving scenario would add, so the phase2-baseline command can measure the
# same scene once standing still and once streaming.
function Get-ScenarioArguments {
    param(
        [Parameter(Mandatory)][string]$Name,
        [Parameter(Mandatory)][string]$RunDirectory,
        [Parameter(Mandatory)][string]$Commit,
        [Parameter(Mandatory)][string]$Hardware,
        [string]$RunId = "run-1",
        [string]$RunLabel,
        [switch]$DirtyWorktree,
        [switch]$StandingStill,
        [int]$DurationSeconds = 120,
        [int]$StabilitySeconds = 1800,
        [string]$ResolvedAssets,
        [string]$Worldspace = "0x3c",
        [int]$RuralGridX = 0,
        [int]$RuralGridY = 0,
        [int]$DenseGridX = 3,
        [int]$DenseGridY = -12,
        [int]$DenseLegacyGridX = 18,
        [int]$DenseLegacyGridY = -5,
        [int]$WaterGridX = 0,
        [int]$WaterGridY = 0
    )
    $arguments = @(
        "--benchmark-duration", $(if ($Name -eq "stability") { "$StabilitySeconds" } else { "$DurationSeconds" }),
        "--benchmark-output", (Join-Path $RunDirectory "acceptance.json"),
        "--profile-output", $RunDirectory,
        "--profile-scenario", $Name,
        "--profile-run-id", $RunId,
        "--profile-commit", $Commit,
        "--profile-hardware", $Hardware,
        "--accept-min-fps", "0",
        "--accept-p95-ms", "1000000",
        "--accept-max-memory-growth-gib", "1000000"
    )
    if ($RunLabel) { $arguments += @("--run-label", $RunLabel) }
    if ($DirtyWorktree) { $arguments += "--profile-dirty-worktree" }
    if ($Name -eq "synthetic") {
        $arguments += @("--benchmark-only", "--synthetic-instances", "250000")
    } else {
        $arguments += @("--assets", $ResolvedAssets, "--worldspace", $Worldspace)
        if ($Name -eq "water") {
            $arguments += @("--grid-x", "$WaterGridX", "--grid-y", "$WaterGridY")
        } elseif ($Name -eq "dense-legacy") {
            # The old dense place (18,-5), kept for one release; compare older `dense` medians with it by hand.
            $arguments += @("--grid-x", "$DenseLegacyGridX", "--grid-y", "$DenseLegacyGridY")
        } elseif ($Name -in @("dense", "stability")) {
            $arguments += @("--grid-x", "$DenseGridX", "--grid-y", "$DenseGridY")
        } else {
            $arguments += @("--grid-x", "$RuralGridX", "--grid-y", "$RuralGridY")
        }
        if ($Name -eq "stress") {
            $arguments += @("--stream-radius", "3")
            if (-not $StandingStill) { $arguments += @("--auto-fly-speed", "5000") }
        }
        if ($Name -eq "stability" -and -not $StandingStill) {
            $arguments += @("--auto-fly-speed", "900")
        }
    }
    return $arguments
}

# The value of `--<Name>` in an engine argument list, or `$null` when the switch is not there.
function Get-ArgumentValue {
    param([string[]]$Arguments, [string]$Name)
    for ($index = 0; $index -lt ($Arguments.Count - 1); $index++) {
        if ($Arguments[$index] -eq $Name) { return $Arguments[$index + 1] }
    }
    return $null
}

# Where a scenario's camera ran, as the string a stored baseline records for it: `<worldspace>
# <gridX>,<gridY>` for a world scene, taken from the arguments the engine was actually started with,
# and `synthetic` for a scene that runs in no world place (the fixture and synthetic scenes).
#
# A scene's size is a property of the place's data, not of the scenario's name: `dense` moved from
# 18,-5 to Riverwood (3,-12, about twice as dense), so a baseline entry stored under the name `dense`
# can describe a place this run did not measure.
function Get-ScenarioPlace {
    param([Parameter(Mandatory)][string[]]$Arguments)
    $worldspace = Get-ArgumentValue -Arguments $Arguments -Name "--worldspace"
    $gridX = Get-ArgumentValue -Arguments $Arguments -Name "--grid-x"
    $gridY = Get-ArgumentValue -Arguments $Arguments -Name "--grid-y"
    if (-not $worldspace -or $null -eq $gridX -or $null -eq $gridY) { return "synthetic" }
    return "$worldspace $gridX,$gridY"
}

function Get-Median {
    param([double[]]$Values)
    $ordered = @($Values | Sort-Object)
    $middle = [int][Math]::Floor($ordered.Count / 2)
    if ($ordered.Count % 2) { return $ordered[$middle] }
    return ($ordered[$middle - 1] + $ordered[$middle]) / 2.0
}

function Get-RegressionComparison {
    param([string]$Metric, [double]$Old, [double]$New)
    $absoluteDelta = if ($Metric -eq "average_fps") { $Old - $New } else { $New - $Old }
    $regression = $absoluteDelta / $Old * 100.0
    $noiseFloor = switch ($Metric) {
        "average_fps" { 5.0 }
        { $_ -in @("frame_ms_p95", "frame_ms_p99") } { 1.5 }
        { $_ -in @("peak_process_memory_gib", "process_memory_growth_gib") } { 0.05 }
        default { 0.0 }
    }
    $materialRegression = $absoluteDelta -gt $noiseFloor
    $status = if ($materialRegression -and $regression -gt 10.0) {
        "fail"
    } elseif ($materialRegression -and $regression -gt 5.0) {
        "warn"
    } else {
        "pass"
    }
    return [ordered]@{
        regression_percent = $regression
        regression_absolute = $absoluteDelta
        noise_floor = $noiseFloor
        status = $status
    }
}

# Runs one window of one scenario and returns its report (`frame-metrics.json`, the JSON the engine
# also writes as `acceptance.json`). Throws when the engine exits non-zero, and when the report it
# reads back cannot be shown to have been written by this invocation: a successful exit with a stale
# or absent report would otherwise count an old run (or nothing) as a fresh repeat.
#
# `-AllowGateFailure` keeps every one of those report checks but not the exit code as a failure of its
# own: the engine exits non-zero when a window's acceptance gates fail, and that window did measure.
# The report is returned only when its own `passed` agrees the run failed; a report that says the run
# passed means the engine failed for some other reason, which no campaign may count as a repeat.
function Invoke-EngineWindow {
    param(
        [Parameter(Mandatory)][string]$Engine,
        [Parameter(Mandatory)][string[]]$Arguments,
        [Parameter(Mandatory)][string]$RunDirectory,
        [Parameter(Mandatory)][string]$Label,
        [switch]$AsHashtable,
        [switch]$AllowGateFailure
    )
    New-Item -ItemType Directory -Force -Path $RunDirectory | Out-Null
    $reportPath = Join-Path $RunDirectory "frame-metrics.json"
    $metadataPath = Join-Path $RunDirectory "metadata.json"
    # What this invocation asked the engine to run, so the bundle read back can be identified as this
    # run's rather than an earlier one's.
    $expectedRunId = Get-ArgumentValue $Arguments "--profile-run-id"
    $expectedScenario = Get-ArgumentValue $Arguments "--profile-scenario"
    $startedUnixMs = [DateTimeOffset]::UtcNow.ToUnixTimeMilliseconds()
    # The engine logs to stderr; under `$ErrorActionPreference = "Stop"` PowerShell 5.1 would treat
    # the first log line as an error, so the preference is relaxed for the run. The engine runs from
    # the repository root: its default assets directory, ini files and screenshot paths are resolved
    # relative to the working directory, so a direct campaign must not depend on where it was started.
    $previous = $ErrorActionPreference
    $ErrorActionPreference = "Continue"
    Push-Location $script:repository
    try {
        & $Engine @Arguments
        $exitCode = $LASTEXITCODE
    } finally {
        Pop-Location
        $ErrorActionPreference = $previous
    }
    if ($exitCode -ne 0 -and -not $AllowGateFailure) { throw "$Label failed with exit code $exitCode" }
    # How the report checks below name the exit they found no report behind. The engine's exit code is
    # read rather than assumed: with -AllowGateFailure a repeat can reach them after exiting non-zero.
    $exitNote = if ($exitCode -eq 0) { "exited 0" } else { "exited $exitCode" }
    if (-not (Test-Path -LiteralPath $reportPath -PathType Leaf)) {
        throw "$Label $exitNote but wrote no report in $RunDirectory`: the repeat measured nothing"
    }
    $report = Get-Content -LiteralPath $reportPath -Raw | ConvertFrom-Json -AsHashtable
    if ($report.format_version -ne $script:benchmarkReportFormatVersion) {
        throw "$Label wrote report format $($report.format_version) in $RunDirectory, but this script reads format $($script:benchmarkReportFormatVersion)"
    }
    if ([long]$report.generated_unix_ms -lt $startedUnixMs) {
        throw "$Label read a stale report from $RunDirectory (written at $($report.generated_unix_ms) ms, this run started at $startedUnixMs ms)"
    }
    if ([long]$report.frames -le 0 -or [double]$report.elapsed_seconds -le 0) {
        throw "$Label reported no measured frames in $RunDirectory (frames $($report.frames), elapsed $($report.elapsed_seconds) s), so the repeat did not complete"
    }
    if (Test-Path -LiteralPath $metadataPath -PathType Leaf) {
        $metadata = Get-Content -LiteralPath $metadataPath -Raw | ConvertFrom-Json -AsHashtable
        if ($expectedRunId -and $metadata.run_id -ne $expectedRunId) {
            throw "$Label read a bundle in $RunDirectory belonging to run '$($metadata.run_id)', not '$expectedRunId'"
        }
        if ($expectedScenario -and $metadata.scenario -ne $expectedScenario) {
            throw "$Label read a bundle in $RunDirectory belonging to scenario '$($metadata.scenario)', not '$expectedScenario'"
        }
    } elseif ($expectedRunId) {
        throw "$Label $exitNote but wrote no profiling bundle in $RunDirectory (no metadata.json), so the repeat cannot be identified"
    }
    # Reached with a non-zero exit only under -AllowGateFailure, and the report is about to stand for
    # that exit: it must be the gate failure the engine exited over. A missing `passed` is not a false
    # one, and neither is `true` — both mean the engine failed for another reason.
    if ($exitCode -ne 0 -and $report.passed -ne $false) {
        throw "$Label exited $exitCode but its report says the run passed: the engine failed for another reason"
    }
    if ($AsHashtable) { return $report }
    return (Get-Content -LiteralPath $reportPath -Raw | ConvertFrom-Json)
}

if ($AsLibrary) { return }

$engine = Get-EngineBinaryPath
$stamp = Get-Date -Format "yyyyMMdd-HHmmss"
$hardware = Get-BenchmarkHardware
$campaign = Join-Path $script:repository "target\profiling\$stamp-$hardware"
$testTemp = Join-Path $script:repository "target\test-temp"
New-Item -ItemType Directory -Force -Path $campaign, $testTemp | Out-Null
$env:TEMP = $testTemp
$env:TMP = $testTemp

if ($Quick) {
    $DurationSeconds = 3
    $StabilitySeconds = 5
    $Repetitions = 1
}
if ($Repetitions -lt 1) { throw "Repetitions must be at least 1" }
$benchmarkPriority = [Diagnostics.ProcessPriorityClass]::AboveNormal
try {
    (Get-Process -Id $PID).PriorityClass = $benchmarkPriority
} catch {
    throw "Could not set the profiling runner priority to $benchmarkPriority`: $($_.Exception.Message)"
}

if (-not $SkipBuild -or -not (Test-Path -LiteralPath $engine -PathType Leaf)) {
    Invoke-EngineReleaseBuild -Jobs 1
}

$identity = Get-BenchmarkRunIdentity
$commit = $identity.commit
$dirty = $identity.dirty
$baselinePath = if ([IO.Path]::IsPathRooted($Baseline)) { $Baseline } else { Join-Path $script:repository $Baseline }
$baselineData = if (Test-Path -LiteralPath $baselinePath) {
    Get-Content -LiteralPath $baselinePath -Raw | ConvertFrom-Json -AsHashtable
} else { @{} }

$resolvedAssets = Resolve-ConvertedAssetSet -Assets $Assets

$requested = if ($Scenario -eq "all") {
    if ($resolvedAssets) { @("synthetic", "rural", "dense", "water", "stress", "stability") } else { @("synthetic") }
} else { @($Scenario) }
if ($requested.Where({ $_ -ne "synthetic" }).Count -gt 0 -and -not $resolvedAssets) {
    throw "Scenario '$Scenario' requires -Assets with a converted legal Skyrim asset set"
}

$results = @{}
$regressionFailed = $false
foreach ($name in $requested) {
    $runs = @()
    $place = $null
    for ($run = 1; $run -le $Repetitions; $run++) {
        $runDirectory = Join-Path $campaign "$name\run-$run"
        $arguments = Get-ScenarioArguments -Name $name -RunDirectory $runDirectory -Commit $commit -Hardware $hardware `
            -RunId "run-$run" -DirtyWorktree:$dirty -DurationSeconds $DurationSeconds -StabilitySeconds $StabilitySeconds `
            -ResolvedAssets $resolvedAssets -Worldspace $Worldspace `
            -RuralGridX $RuralGridX -RuralGridY $RuralGridY -DenseGridX $DenseGridX -DenseGridY $DenseGridY `
            -DenseLegacyGridX $DenseLegacyGridX -DenseLegacyGridY $DenseLegacyGridY `
            -WaterGridX $WaterGridX -WaterGridY $WaterGridY
        # Read from the first run's arguments: every repeat of a scenario is started the same way, and
        # the arguments are what the engine was actually given.
        if ($run -eq 1) { $place = Get-ScenarioPlace -Arguments $arguments }
        $runs += Invoke-EngineWindow -Engine $engine -Arguments $arguments -RunDirectory $runDirectory -Label "Profiling run $name/run-$run"
    }

    $median = [ordered]@{
        # Where these medians were measured, stored beside them in the baseline (see
        # Get-ScenarioPlace): a scenario is compared by name, and the entry a baseline holds for that
        # name only describes this run's numbers when it was measured in the same place.
        place = $place
        average_fps = Get-Median @($runs.average_fps)
        frame_ms_p95 = Get-Median @($runs.frame_ms_p95)
        frame_ms_p99 = Get-Median @($runs.frame_ms_p99)
        peak_process_memory_gib = Get-Median @($runs.peak_process_memory_gib)
        process_memory_growth_gib = Get-Median @($runs.process_memory_growth_gib)
    }
    $comparison = [ordered]@{ scenario = $name; repetitions = $Repetitions; median = $median; regressions = @() }
    if ($baselineData.ContainsKey($name)) {
        # A stored entry measured in another place — or one written before places were recorded at all
        # — does not describe this run's numbers, so the scenario is not scored against it: the
        # comparison is skipped, with the reason in one warning line and in `comparison.json`, and the
        # campaign is not failed for it. Scoring such an entry reports a fake regression: `dense` is
        # Riverwood now, about twice as dense as the place its older entry measured.
        $storedEntry = $baselineData[$name]
        $storedPlace = $null
        if ($storedEntry -is [System.Collections.IDictionary] -and $storedEntry.Contains("place")) { $storedPlace = $storedEntry.place }
        $skipReason = if ($null -eq $storedPlace) {
            'the stored baseline records no place for it (it was written before places were stored, and `dense` has since moved to Riverwood); rerun with -UpdateBaseline to store one'
        } elseif ([string]$storedPlace -ne [string]$place) {
            "stored place $storedPlace, run place $place"
        } else {
            $null
        }
        if ($skipReason) {
            $comparison.baseline_skipped = $skipReason
            Write-Warning "Not comparing ${name} with ${baselinePath}: $skipReason"
        } else {
            foreach ($metric in @("average_fps", "frame_ms_p95", "frame_ms_p99", "peak_process_memory_gib", "process_memory_growth_gib")) {
                $old = [double]$baselineData[$name][$metric]
                $new = [double]$median[$metric]
                if ($old -le 0) { continue }
                $regression = Get-RegressionComparison -Metric $metric -Old $old -New $new
                if ($regression.status -eq "fail") { $regressionFailed = $true }
                $comparison.regressions += [ordered]@{
                    metric = $metric; baseline = $old; current = $new
                    regression_percent = $regression.regression_percent
                    regression_absolute = $regression.regression_absolute
                    noise_floor = $regression.noise_floor
                    status = $regression.status
                }
            }
        }
    }
    $results[$name] = $median
    $comparison | ConvertTo-Json -Depth 6 | Set-Content -LiteralPath (Join-Path $campaign "$name\comparison.json")
}

[ordered]@{ format_version = 1; generated_at = (Get-Date).ToString("o"); commit = $commit; dirty = $dirty; hardware = $hardware; scenarios = $results } |
    ConvertTo-Json -Depth 8 | Set-Content -LiteralPath (Join-Path $campaign "campaign.json")
$summary = @(
    "# Phase 2 profiling campaign",
    "",
    "- Commit: $commit",
    "- Dirty worktree: $dirty",
    "- Hardware: $hardware",
    "- Repetitions: $Repetitions",
    "",
    "| Scenario | Median FPS | Frame P95 ms | Frame P99 ms | Peak GiB | Growth GiB |",
    "|---|---:|---:|---:|---:|---:|"
)
foreach ($name in $requested) {
    $value = $results[$name]
    $summary += "| $name | $([Math]::Round($value.average_fps, 2)) | $([Math]::Round($value.frame_ms_p95, 3)) | $([Math]::Round($value.frame_ms_p99, 3)) | $([Math]::Round($value.peak_process_memory_gib, 3)) | $([Math]::Round($value.process_memory_growth_gib, 3)) |"
}
$summary | Set-Content -LiteralPath (Join-Path $campaign "summary.md")
if ($UpdateBaseline) {
    $results | ConvertTo-Json -Depth 6 | Set-Content -LiteralPath $baselinePath
    Write-Host "Baseline updated: $baselinePath"
}
if ($regressionFailed) { throw "Profiling regression exceeded 10%. See $campaign" }
Write-Host "Profiling campaign complete: $campaign"
