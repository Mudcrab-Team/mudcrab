[CmdletBinding()]
param(
    # Which scenes to measure. The default is the six the baseline compares: the synthetic scene
    # is the one a machine with no converted data can run, the four world scenes are the walk, the
    # dense place (Riverwood), the water and the streaming stress; with -Assets it also runs `travel`,
    # the route the frame-time goal is measured on (scripts/routes/travel.json). `stability` is left
    # out because it is a soak window (30 minutes by default), not a baseline; `all` selects it too.
    # `check` is the held-out route (scripts/routes/check.json): reported, but never in the defaults
    # and never tuned against; `all` selects it too.
    [ValidateSet("all", "synthetic", "rural", "dense", "dense-legacy", "water", "stress", "stability", "travel", "check")]
    [string[]]$Scenarios = @(),
    # The converted legal Skyrim asset set every world scenario loads. Not needed for `synthetic`.
    [string]$Assets,
    [int]$Repeats = 3,
    # Every window runs this long, except `stability`, which uses -StabilitySeconds. A route window
    # ends when its route does, and runs at least as long as its travel time plus a readiness
    # allowance (see `Get-WindowDuration`), so a route cannot be cut short by the world loading.
    [int]$DurationSeconds = 120,
    [int]$StabilitySeconds = 1800,
    [string]$Out,
    [switch]$NoBuild,
    # A previous baseline.json to compare against. Without it, an existing `baseline.json` in -Out
    # is used. The comparison is refused when the two runs' renderer backend or adapter differ.
    [string]$CompareTo,
    [string]$Worldspace = "0x3c",
    # The frame time the report counts frames over, passed to every window. The default is 6.25 ms,
    # 160 frames per second; the proposed bar is 99.9 percent of frames within it and none over twice it.
    [ValidateRange(0.001, 1000)]
    [double]$FrameBudgetMs = 6.25,
    # The standard places of docs/roadmap/02-acceptance.md, so the three world scenes measure three
    # different places by default: the phase2-profile.ps1 defaults are 0,0 for rural and water.
    # Dense is Riverwood (about 2,900 references within two cells); `dense-legacy` is the old dense
    # place 18,-5 (about 1,530), kept for one release and not in the default set.
    [int]$RuralGridX = 0,
    [int]$RuralGridY = 0,
    [int]$DenseGridX = 3,
    [int]$DenseGridY = -12,
    [int]$DenseLegacyGridX = 18,
    [int]$DenseLegacyGridY = -5,
    [int]$WaterGridX = 7,
    [int]$WaterGridY = -2
)

$ErrorActionPreference = "Stop"
if ($PSVersionTable.PSVersion.Major -lt 7) {
    throw "scripts/phase2-baseline.ps1 needs PowerShell 7 (pwsh): run it as 'pwsh -File scripts/phase2-baseline.ps1 ...'"
}
# The scenario definitions, the engine invocation and the regression helper live in
# phase2-profile.ps1; -AsLibrary imports them without starting a campaign, so both commands run
# the engine the same way.
#
# Dot-sourcing runs that file in this scope, so its own parameter defaults overwrite the same
# names here (-Assets, the window lengths, -Worldspace and the grids). The values this command was
# called with are put back afterwards, or a caller's -Assets and -DurationSeconds would be lost.
$sharedNames = @(
    "Assets", "DurationSeconds", "StabilitySeconds", "Worldspace",
    "RuralGridX", "RuralGridY", "DenseGridX", "DenseGridY", "DenseLegacyGridX", "DenseLegacyGridY", "WaterGridX", "WaterGridY"
)
$requestedValues = @{}
foreach ($name in $sharedNames) { $requestedValues[$name] = Get-Variable -Name $name -ValueOnly }
. (Join-Path $PSScriptRoot "phase2-profile.ps1") -AsLibrary
foreach ($name in $sharedNames) { Set-Variable -Name $name -Value $requestedValues[$name] }

$defaultScenarios = @("synthetic", "rural", "dense", "water", "stress", "travel")
$requested = if ($Scenarios.Count -eq 0) {
    # Like the profiling campaign, the default without -Assets is the one scene that needs no
    # converted data instead of a request that immediately fails.
    if ($Assets) { $defaultScenarios } else { @("synthetic") }
} elseif ($Scenarios -contains "all") {
    # `all` is an explicit request for every scenario, the world ones included, so unlike the bare
    # default it is not narrowed to the scene that needs no converted data: without an asset set it
    # is refused here, naming the missing input, before a campaign is planned.
    if (-not $Assets) {
        throw "-Scenarios all needs -Assets with a converted legal Skyrim asset set: all eight scenarios include the world scenes and the two routes. The bare default without -Assets runs 'synthetic' only"
    }
    @("synthetic", "rural", "dense", "water", "stress", "stability", "travel", "check")
} else { $Scenarios }
if ($Repeats -lt 1) { throw "Repeats must be at least 1" }
if ($requested.Where({ $_ -ne "synthetic" }).Count -gt 0 -and -not $Assets) {
    throw "Scenario $(($requested -ne 'synthetic') -join ', ') requires -Assets with a converted legal Skyrim asset set"
}

$repository = Get-ProfilingRepository
$engine = Get-EngineBinaryPath
if (-not $Out) { $Out = Join-Path $repository "target\phase2-baseline" }
$Out = if ([IO.Path]::IsPathRooted($Out)) { $Out } else { Join-Path $repository $Out }
$testTemp = Join-Path $Out "tmp"
New-Item -ItemType Directory -Force -Path $Out, $testTemp | Out-Null
$env:TEMP = $testTemp
$env:TMP = $testTemp

$resolvedAssets = Resolve-ConvertedAssetSet -Assets $Assets

# The route scenarios: the camera follows a route file (scripts/routes) at the file's speed, so the
# window measures travelling. The engine knows no route; the scenario only names the file. Each has
# one window (the moving one): the scene standing still is the other scenarios' job.
$routeScenarios = @{ travel = "travel.json"; check = "check.json" }
# The route starts only once the world is ready (`"start": "world_ready"`), and the engine counts the
# window's duration from the process start, so a route window also allows this for the world to load.
$RouteReadinessSeconds = 60

function Get-RouteFilePath([string]$Name) {
    return Join-Path $PSScriptRoot "routes\$($routeScenarios[$Name])"
}

# The seconds a route window needs to walk its route at the file's speed: the length of its legs
# over the speed, rounded up. A looped route has no end, so it has no travel time to allow for.
function Get-RouteTravelSeconds([string]$Name) {
    $route = Get-Content -LiteralPath (Get-RouteFilePath $Name) -Raw | ConvertFrom-Json
    if ($route.loop) { throw "Route $Name loops, so it has no travel time for its window to allow for" }
    $length = 0.0
    $points = @($route.waypoints)
    for ($index = 1; $index -lt $points.Count; $index++) {
        $dx = [double]$points[$index][0] - [double]$points[$index - 1][0]
        $dy = [double]$points[$index][1] - [double]$points[$index - 1][1]
        $length += [Math]::Sqrt($dx * $dx + $dy * $dy)
    }
    return [int][Math]::Ceiling($length / [double]$route.speed)
}

# The engine arguments of one window: the profiling campaign's arguments for the scenario, with the
# route scenarios built from the rural scene's arguments (the route file, not a grid, says where
# the run starts), and the frame budget passed to every window.
function Get-WindowArguments([string]$Name, [hashtable]$Parameters) {
    if (-not $routeScenarios.ContainsKey($Name)) {
        $arguments = @(Get-ScenarioArguments -Name $Name @Parameters)
    } else {
        $base = @(Get-ScenarioArguments -Name "rural" @Parameters)
        $arguments = @()
        for ($index = 0; $index -lt $base.Count; $index++) {
            # The route moves the camera itself, so the rural window's grid and flight speed go.
            if ($base[$index] -in @("--grid-x", "--grid-y", "--auto-fly-speed")) { $index++; continue }
            if ($base[$index] -eq "--profile-scenario") {
                $arguments += @("--profile-scenario", $Name)
                $index++
                continue
            }
            $arguments += $base[$index]
        }
        $arguments += @("--route", (Get-RouteFilePath $Name))
    }
    return $arguments + @("--frame-budget-ms", "$FrameBudgetMs")
}

# The windows a scenario is measured in: a route scenario only its moving window, a flying scenario
# a standing-still window and a streaming window, everything else one standing-still window.
function Get-WindowKinds([string]$Name, $WindowCounts) {
    if ($routeScenarios.ContainsKey($Name)) { return @("streaming") }
    if ($WindowCounts[$Name] -eq 2) { return @("standing_still", "streaming") }
    return @("standing_still")
}

# The parameters of one window's engine arguments. `$Run` adds the per-run fields (run directory,
# label, identity) that the planner does not need and the window loop does.
function Get-WindowInvocationParameters([string]$Name, [string]$Kind, [hashtable]$Run) {
    $parameters = @{
        StandingStill = ($Kind -eq "standing_still")
        DurationSeconds = (Get-WindowDuration $Name); StabilitySeconds = $StabilitySeconds
        ResolvedAssets = $resolvedAssets; Worldspace = $Worldspace
        RuralGridX = $RuralGridX; RuralGridY = $RuralGridY; DenseGridX = $DenseGridX; DenseGridY = $DenseGridY
        DenseLegacyGridX = $DenseLegacyGridX; DenseLegacyGridY = $DenseLegacyGridY
        WaterGridX = $WaterGridX; WaterGridY = $WaterGridY
    }
    foreach ($key in $Run.Keys) { $parameters[$key] = $Run[$key] }
    return $parameters
}

# The workload each window of the campaign will be started with, known before any engine runs, keyed
# `scenario/kind`. The planner builds the same engine arguments the window loop does.
function Get-PlannedWorkloads($Names, $WindowCounts) {
    $planned = [ordered]@{}
    foreach ($name in $Names) {
        foreach ($kind in (Get-WindowKinds $name $WindowCounts)) {
            $arguments = Get-WindowArguments $name (Get-WindowInvocationParameters $name $kind @{
                RunDirectory = "."; Commit = "plan"; Hardware = "plan"; RunId = "plan"; DirtyWorktree = $false
            })
            $planned["$name/$kind"] = Get-KnownScenarioInputs $arguments (Get-WindowDuration $name)
        }
    }
    return $planned
}

function Get-WindowDuration([string]$Name) {
    if ($Name -eq "stability") { return $StabilitySeconds }
    if ($routeScenarios.ContainsKey($Name)) {
        return [Math]::Max($DurationSeconds, (Get-RouteTravelSeconds $Name) + $RouteReadinessSeconds)
    }
    return $DurationSeconds
}

# A metric's spread across the repeats: the median the baseline compares on, and the smallest and
# largest value seen, so a reader can tell a stable number from a lucky one. `$null` when none of
# the repeats measured it (the report never zero-fills, and neither does this).
function Get-Spread($Values) {
    $numbers = @($Values | Where-Object { $null -ne $_ } | ForEach-Object { [double]$_ })
    if ($numbers.Count -eq 0) { return $null }
    $ordered = @($numbers | Sort-Object)
    return [ordered]@{
        median = Get-Median $numbers
        min = $ordered[0]
        max = $ordered[-1]
        samples = $numbers.Count
        runs = @($Values).Count
    }
}

function Get-Values($Reports, [scriptblock]$Selector) {
    return @($Reports | ForEach-Object { & $Selector $_ })
}

# The spread, across the repeats, of the `mean` of each entry of a `name -> MetricSummary` map
# (the report's GPU passes, CPU schedules and CPU spans). The names are the union of every repeat's
# keys, so a pass or span only a later repeat measured is still reported, with the sample count
# telling the reader how many repeats measured it.
function Get-SummaryMap($Reports, [scriptblock]$Selector) {
    $names = [ordered]@{}
    foreach ($report in $Reports) {
        $map = & $Selector $report
        if ($map) { foreach ($key in @($map.Keys)) { $names[$key] = $true } }
    }
    $summary = [ordered]@{}
    foreach ($name in @($names.Keys)) {
        $values = foreach ($report in $Reports) {
            $map = & $Selector $report
            if ($map -and $map.ContainsKey($name)) { $map[$name].mean } else { $null }
        }
        $summary[$name] = Get-Spread $values
    }
    return $summary
}

# The engine's own sample count behind each entry of a `name -> MetricSummary` map, as a spread over
# the repeats. This is the count the window's `measured_frames` is set against: smaller means the
# pass or counter was not sampled on every measured frame (GPU readback lag, or a pass that did not
# run every frame), larger that several cameras ran the same pass.
function Get-CountMap($Reports, [scriptblock]$Selector) {
    # The inner scriptblock must call the *caller's* selector, not itself; see Get-SpreadPerName.
    $select = $Selector
    $names = [ordered]@{}
    foreach ($report in $Reports) {
        $map = & $select $report
        if ($map) { foreach ($key in @($map.Keys)) { $names[$key] = $true } }
    }
    $counts = [ordered]@{}
    foreach ($name in @($names.Keys)) {
        $values = foreach ($report in $Reports) {
            $map = & $select $report
            # Indexed with `["count"]` rather than read as `.count`: a hashtable without a `count`
            # key answers a property of that name with its key count, which is not a sample count.
            if ($map -and $map.ContainsKey($name)) { $map[$name]["count"] } else { $null }
        }
        $counts[$name] = Get-Spread $values
    }
    return $counts
}

# The reasons a section could not measure something, unioned across the repeats: a key any repeat
# reports as unavailable is unavailable for the window, and every distinct reason is kept rather
# than only the first repeat's.
function Get-UnavailableMap($Reports, [scriptblock]$Selector) {
    $perKey = [ordered]@{}
    foreach ($report in $Reports) {
        $map = & $Selector $report
        if (-not $map) { continue }
        foreach ($key in @($map.Keys)) {
            $reason = [string]$map[$key]
            if (-not $perKey.Contains($key)) { $perKey[$key] = @() }
            if ($perKey[$key] -notcontains $reason) { $perKey[$key] += $reason }
        }
    }
    $merged = [ordered]@{}
    foreach ($key in @($perKey.Keys)) { $merged[$key] = ($perKey[$key] -join "; ") }
    return $merged
}

# The fields of a fingerprint every window of one campaign must share: the machine, the build and
# the world-data schemas the numbers were measured on. Everything that legitimately differs between
# windows — the effective graphics settings (a stream-radius override), which database a scenario
# opened, the commit being benchmarked — is kept per window instead.
function Get-FingerprintInvariant($Fingerprint) {
    if ($null -eq $Fingerprint) { return $null }
    $renderer = $Fingerprint.renderer
    return [ordered]@{
        engine_version = $Fingerprint.engine_version
        bevy_version = $Fingerprint.bevy_version
        build_profile = $Fingerprint.build_profile
        resolution = $Fingerprint.resolution
        world_database_schema = $Fingerprint.world_database_schema
        cell_cache_version = $Fingerprint.cell_cache_version
        renderer = if ($null -eq $renderer) { $null } else { [ordered]@{
                backend = $renderer.backend
                adapter = $renderer.adapter
                driver = $renderer.driver
                driver_info = $renderer.driver_info
                device_type = $renderer.device_type
                timestamp_queries_supported = $renderer.timestamp_queries_supported
                pipeline_statistics_supported = $renderer.pipeline_statistics_supported
            } }
    }
}

# The differences between two maps of scalars (nested maps are compared one level deep), as
# human-readable `field : old -> new` lines. An empty list means the two describe the same thing.
function Get-FieldDifferences($Old, $New, [string]$Prefix = "") {
    $names = [ordered]@{}
    foreach ($name in @($Old.Keys)) { $names[$name] = $true }
    foreach ($name in @($New.Keys)) { $names[$name] = $true }
    $differences = @()
    foreach ($name in @($names.Keys)) {
        $oldValue = $Old[$name]
        $newValue = $New[$name]
        $path = if ($Prefix) { "$Prefix.$name" } else { "$name" }
        if ($oldValue -is [System.Collections.IDictionary] -or $newValue -is [System.Collections.IDictionary]) {
            if ($null -eq $oldValue -or $null -eq $newValue) {
                if ($null -ne $oldValue -or $null -ne $newValue) { $differences += "$path : $(Format-FieldValue $oldValue) -> $(Format-FieldValue $newValue)" }
                continue
            }
            $differences += Get-FieldDifferences $oldValue $newValue $path
            continue
        }
        if ((Format-FieldValue $oldValue) -ne (Format-FieldValue $newValue)) {
            $differences += "$path : $(Format-FieldValue $oldValue) -> $(Format-FieldValue $newValue)"
        }
    }
    return $differences
}

function Format-FieldValue($Value) {
    if ($null -eq $Value) { return "null" }
    # ConvertFrom-Json turns ISO 8601 strings into dates; compare them in the form they were written.
    if ($Value -is [datetime]) { return $Value.ToUniversalTime().ToString("o") }
    if ($Value -is [datetimeoffset]) { return $Value.UtcDateTime.ToString("o") }
    return "$Value"
}

# What the converted data set a window loaded actually is. The directory path identifies nothing by
# itself — a warm re-conversion, or an added mod, leaves the path and the schema unchanged — so the
# window also records two things that do change with the data: the size and last-write time of the
# world database (`skyrim_world.db`) and the size of the cell cache (`cell_cache.rkyv`), which a
# re-conversion or a different plugin set rewrites, and the conversion manifest's own
# `configuration_hash` and `schema_version`, which say what settings the converter ran with. A copy of
# the same data set to another folder keeps the sizes but not the last-write time, and is refused too:
# a refusal is the safe direction, and `-CompareTo` names the baseline to rerun against. A data set
# that could not be read (the window was started without `--assets`, a file is missing, or the
# manifest does not parse) is `null` with the reason returned beside it, never a guessed identity.
# Read once per data set: the manifest of a full conversion is tens of MiB of JSON, and every window
# of a campaign asks about the same one.
$script:assetsIdentityCache = @{}

function Get-AssetsIdentity([string]$ResolvedAssets) {
    $none = { param($Reason) [ordered]@{
        world_database_bytes = $null; world_database_modified_utc = $null; cell_cache_bytes = $null
        configuration_hash = $null; schema_version = $null; unavailable = $Reason
    } }
    if (-not $ResolvedAssets) {
        return & $none "this window was started without -Assets, so it loaded no converted data set"
    }
    if ($script:assetsIdentityCache.ContainsKey($ResolvedAssets)) { return $script:assetsIdentityCache[$ResolvedAssets] }
    $identity = & {
        $database = Join-Path $ResolvedAssets "skyrim_world.db"
        $cellCache = Join-Path $ResolvedAssets "cell_cache.rkyv"
        $manifestPath = Join-Path $ResolvedAssets "conversion-manifest.json"
        foreach ($required in @($database, $cellCache, $manifestPath)) {
            if (-not (Test-Path -LiteralPath $required -PathType Leaf)) {
                return & $none "the data set at $ResolvedAssets has no $(Split-Path -Leaf $required)"
            }
        }
        try {
            $manifest = Get-Content -LiteralPath $manifestPath -Raw | ConvertFrom-Json -AsHashtable
        } catch {
            return & $none "conversion-manifest.json could not be read ($($_.Exception.Message))"
        }
        if ($manifest -isnot [System.Collections.IDictionary]) {
            return & $none "conversion-manifest.json is not a JSON object"
        }
        $databaseInfo = Get-Item -LiteralPath $database
        $configurationHash = if ($manifest.Contains("configuration_hash")) { $manifest.configuration_hash } else { $null }
        $schemaVersion = if ($manifest.Contains("schema_version")) { $manifest.schema_version } else { $null }
        # An empty hash is what a manifest written before the producer recorded one carries: it names
        # no settings either, so it is recorded as unread rather than as an identity.
        $unavailable = $null
        if ([string]::IsNullOrEmpty([string]$configurationHash)) {
            $unavailable = "conversion-manifest.json records no configuration_hash for the data set"
        } elseif ($null -eq $schemaVersion) {
            $unavailable = "conversion-manifest.json records no schema_version for the data set"
        }
        return [ordered]@{
            world_database_bytes = $databaseInfo.Length
            world_database_modified_utc = $databaseInfo.LastWriteTimeUtc.ToString("o")
            cell_cache_bytes = (Get-Item -LiteralPath $cellCache).Length
            configuration_hash = $configurationHash
            schema_version = $schemaVersion
            unavailable = $unavailable
        }
    }
    $script:assetsIdentityCache[$ResolvedAssets] = $identity
    return $identity
}

# The inputs a window's scenario was started with, taken from the engine arguments that produced it
# and the report itself, so two windows' numbers can be shown to describe the same workload: where
# the camera was, how the scenario flew it (`--auto-fly-speed`, absent when the camera was left
# standing still), the synthetic scene's instance count, and the resolved asset set the run loaded.
# The asset set is recorded as its full path, so the same set named by a different relative path is
# the same identity and a different set is a difference — and by its own manifest identity, because
# that path can hold a re-converted or modded set without changing.
function Get-KnownScenarioInputs($Arguments, [int]$DurationSeconds) {
    $worldspace = Get-ArgumentValue $Arguments "--worldspace"
    $gridX = Get-ArgumentValue $Arguments "--grid-x"
    $gridY = Get-ArgumentValue $Arguments "--grid-y"
    $flySpeed = Get-ArgumentValue $Arguments "--auto-fly-speed"
    $syntheticInstances = Get-ArgumentValue $Arguments "--synthetic-instances"
    # The route is identified by its file name and content, so a window measured on an edited route
    # is not compared with one measured on the old file. Null for a scenario with no route, which is
    # also what a window from before routes has.
    $routeArgument = Get-ArgumentValue $Arguments "--route"
    $route = if ($null -eq $routeArgument) { $null } else {
        [ordered]@{
            file = Split-Path -Leaf $routeArgument
            sha256 = (Get-FileHash -Algorithm SHA256 -LiteralPath $routeArgument).Hash
        }
    }
    $assetsIdentity = Get-AssetsIdentity (Get-ArgumentValue $Arguments "--assets")
    return [ordered]@{
        worldspace = $worldspace
        grid = if ($null -eq $gridX -and $null -eq $gridY) { $null } else { [ordered]@{ x = [int]$gridX; y = [int]$gridY } }
        fly_speed = if ($null -eq $flySpeed) { $null } else { [int]$flySpeed }
        synthetic_instances = if ($null -eq $syntheticInstances) { $null } else { [int]$syntheticInstances }
        route = $route
        assets = Get-ArgumentValue $Arguments "--assets"
        assets_identity = [ordered]@{
            world_database_bytes = $assetsIdentity.world_database_bytes
            world_database_modified_utc = $assetsIdentity.world_database_modified_utc
            cell_cache_bytes = $assetsIdentity.cell_cache_bytes
            configuration_hash = $assetsIdentity.configuration_hash
            schema_version = $assetsIdentity.schema_version
        }
        assets_identity_unavailable = $assetsIdentity.unavailable
        duration_seconds = $DurationSeconds
    }
}

# The keys of `Get-KnownScenarioInputs`: everything about a window's workload that is fixed before its
# engine starts. The stream radius and the warm-up come from the report and are checked after a run.
$script:knownInputKeys = @(
    "worldspace", "grid", "fly_speed", "synthetic_instances", "route", "assets", "assets_identity",
    "assets_identity_unavailable", "duration_seconds"
)

function Get-ScenarioInputs($Arguments, $Report, [int]$DurationSeconds) {
    $known = Get-KnownScenarioInputs $Arguments $DurationSeconds
    return [ordered]@{
        worldspace = $known.worldspace
        grid = $known.grid
        fly_speed = $known.fly_speed
        synthetic_instances = $known.synthetic_instances
        route = $known.route
        assets = $known.assets
        assets_identity = $known.assets_identity
        assets_identity_unavailable = $known.assets_identity_unavailable
        stream_radius = $Report.fingerprint.graphics.stream_radius
        duration_seconds = $known.duration_seconds
        warmup_frames = $Report.warmup_frames
    }
}

# One frame-time metric of one report, by the report's own field name.
function Get-FrameMetric($Report, [string]$Metric) {
    switch ($Metric) {
        "count" { return $Report.frames }
        "mean_ms" { return $Report.frame_ms_mean }
        "p50_ms" { return $Report.frame_ms_p50 }
        "p95_ms" { return $Report.frame_ms_p95 }
        "p99_ms" { return $Report.frame_ms_p99 }
        "worst_ms" { return $Report.frame_ms_worst }
        "average_fps" { return $Report.average_fps }
    }
    return $null
}

# The engine's own acceptance verdict on the repeats of one window. A baseline run measures through a
# failed gate — the engine exits non-zero over one, and the report it wrote is still a measurement — so
# the gate that failed is recorded beside the numbers instead of being lost or hidden. `runs_failed_gates`
# counts the repeats whose report says `passed: false`, `runs` is every repeat of the window, and
# `failed_gates` is the sorted union of the names in those reports' `thresholds` whose value is the
# boolean `false`: the numeric thresholds beside them are neutralised in a baseline run, so a `false`
# boolean is a real gate. A report that does not carry `passed` is not counted as a failed one.
function Get-Acceptance($Reports) {
    $failedRuns = 0
    $failedGates = @()
    foreach ($report in $Reports) {
        if ($report.passed -eq $false) { $failedRuns++ }
        if ($null -eq $report.thresholds) { continue }
        foreach ($name in @($report.thresholds.Keys)) {
            if ($report.thresholds[$name] -is [bool] -and -not $report.thresholds[$name]) { $failedGates += $name }
        }
    }
    return [ordered]@{
        runs_failed_gates = $failedRuns
        runs = $Reports.Count
        failed_gates = @($failedGates | Sort-Object -Unique)
    }
}

# One window of one scenario: what every repeat measured, reduced to medians and spreads, plus the
# provenance that says what the window actually was — the run's fingerprint and the scenario inputs
# it was started with — so it can be set against another run's window. The maps are per pass, per
# schedule and per span; everything the report left `null` stays `null` here, and every section
# carries the report's `unavailable` reasons.
# The shortfall of a route window, or `$null` when every run walked its whole route. A run that never
# started, or stopped short of the route's end, scored only a part of it, which is not the workload the
# window names, so such a window is never scored, published, or compared as a full route.
function Get-RouteShortfall($Window) {
    if ($null -eq $Window -or $null -eq $Window.route) { return $null }
    $completed = [int]$Window.route.runs_completed
    $runs = [int]$Window.runs
    if ($completed -ge $runs) { return $null }
    return "$($Window.route.file): $completed of $runs runs completed the route ($([int]$Window.route.runs_started) started)"
}

function Get-WindowReport($Reports, [string]$Kind, $Inputs) {
    $first = $Reports[0]
    $window = [ordered]@{
        window = $Kind
        runs = $Reports.Count
        acceptance = Get-Acceptance $Reports
        scenario_inputs = $Inputs
        fingerprint = $first.fingerprint
    }
    foreach ($metric in @("count", "mean_ms", "p50_ms", "p95_ms", "p99_ms", "worst_ms", "average_fps")) {
        $window[$metric] = Get-Spread (Get-Values $Reports { param($report) Get-FrameMetric $report $metric })
    }
    # The frame-time distribution against the budget: every number is the report's own, reduced to
    # the median and spread over the repeats. The owners are counts of the frames over the budget,
    # per run, so they say who the typical run's slow frames belonged to; `unknown` is frames no
    # side could be named for (the report's owner_rule says how and why).
    $budgets = @($Reports | ForEach-Object { $_.frame_budget } | Where-Object { $null -ne $_ })
    if ($budgets.Count -gt 0) {
        $window.frame_budget = [ordered]@{
            budget_ms = $budgets[0].budget_ms
            scope = $budgets[0].scope
            frames = Get-Spread (Get-Values $Reports { param($report) $report.frame_budget.frames })
            p50_ms = Get-Spread (Get-Values $Reports { param($report) $report.frame_budget.p50_ms })
            p99_ms = Get-Spread (Get-Values $Reports { param($report) $report.frame_budget.p99_ms })
            p99_9_ms = Get-Spread (Get-Values $Reports { param($report) $report.frame_budget.p99_9_ms })
            worst_ms = Get-Spread (Get-Values $Reports { param($report) $report.frame_budget.worst_ms })
            frames_over_budget = Get-Spread (Get-Values $Reports { param($report) $report.frame_budget.frames_over_budget })
            over_budget_per_minute = Get-Spread (Get-Values $Reports { param($report) $report.frame_budget.over_budget_per_minute })
            frames_over_double_budget = Get-Spread (Get-Values $Reports { param($report) $report.frame_budget.frames_over_double_budget })
            share_within_budget = Get-Spread (Get-Values $Reports { param($report) $report.frame_budget.share_within_budget })
            runs_meeting_pass_bar = @($budgets | Where-Object { $_.meets_pass_bar -eq $true }).Count
            over_budget_owners = [ordered]@{
                main_world = Get-Spread (Get-Values $Reports { param($report) $report.frame_budget.over_budget_owners.main_world })
                render_thread = Get-Spread (Get-Values $Reports { param($report) $report.frame_budget.over_budget_owners.render_thread })
                gpu = Get-Spread (Get-Values $Reports { param($report) $report.frame_budget.over_budget_owners.gpu })
                upload = Get-Spread (Get-Values $Reports { param($report) $report.frame_budget.over_budget_owners.upload })
                unknown = Get-Spread (Get-Values $Reports { param($report) $report.frame_budget.over_budget_owners.unknown })
            }
            owner_rule = $budgets[0].owner_rule
        }
    } else {
        $window.frame_budget = $null
    }
    # A route window also says how far the route got: a run that did not finish its route (the
    # window ran out first) scored only the part it walked.
    $routes = @($Reports | ForEach-Object { $_.route } | Where-Object { $null -ne $_ })
    $window.route = if ($routes.Count -eq 0) { $null } else {
        [ordered]@{
            # The engine writes the path it was given, and that path is the host's; the window keeps
            # the leaf name only, as `Get-ScenarioInputs` does, so neither the report nor a baseline
            # written here carries the host's directories.
            file = Split-Path -Leaf $routes[0].file
            runs_completed = @($routes | Where-Object { $_.completed -eq $true }).Count
            runs_started = @($routes | Where-Object { $_.started -eq $true }).Count
            length_units = $routes[0].length_units
            speed_units_per_second = $routes[0].speed_units_per_second
            distance_travelled_units = Get-Spread (Get-Values $Reports { param($report) $report.route.distance_travelled_units })
            started_at_run_ms = Get-Spread (Get-Values $Reports { param($report) $report.route.started_at_run_ms })
            frames_on_route = Get-Spread (Get-Values $Reports { param($report) $report.route.frames_on_route })
            route_frames_without_ground = Get-Spread (Get-Values $Reports { param($report) $report.route.route_frames_without_ground })
        }
    }
    $shortfall = Get-RouteShortfall $window
    if ($shortfall) {
        throw "Cannot score route window ${Kind}: $shortfall. A partial route is not the workload the window names; rerun the window with its time limit, or drop it from the selected set"
    }
    $window.gpu_passes = [ordered]@{
        # The frames the window's per-pass numbers rest on, kept beside them so a reader can tell a
        # pass that ran every frame from one that was measured on a few: `pass_samples` below is
        # each pass's own count.
        measured_frames = Get-Spread (Get-Values $Reports { param($report) $report.gpu_passes.measured_frames })
        passes = Get-SummaryMap $Reports { param($report) $report.gpu_passes.passes }
        pass_samples = Get-CountMap $Reports { param($report) $report.gpu_passes.passes }
        unavailable = Get-UnavailableMap $Reports { param($report) $report.gpu_passes.unavailable }
    }
    $window.memory = [ordered]@{
        process_peak_gib = Get-Spread (Get-Values $Reports { param($report) $report.peak_process_memory_gib })
        process_growth_gib = Get-Spread (Get-Values $Reports { param($report) $report.process_memory_growth_gib })
        cpu = [ordered]@{
            mesh_bytes = Get-Spread (Get-Values $Reports { param($report) $report.memory.cpu.mesh_bytes })
            image_bytes = Get-Spread (Get-Values $Reports { param($report) $report.memory.cpu.image_bytes })
        }
        gpu = [ordered]@{
            # These counters are gauges sampled by the render world, so `measured_frames` says how
            # many frames they rest on; it is 0 when the render world never measured them.
            measured_frames = Get-Spread (Get-Values $Reports { param($report) $report.memory.gpu.measured_frames })
            mesh_allocator_slab_bytes = Get-Spread (Get-Values $Reports { param($report) $report.memory.gpu.mesh_allocator_slab_bytes })
            mesh_allocator_slabs = Get-Spread (Get-Values $Reports { param($report) $report.memory.gpu.mesh_allocator_slabs })
            mesh_allocator_allocations = Get-Spread (Get-Values $Reports { param($report) $report.memory.gpu.mesh_allocator_allocations })
            render_asset_counts = Get-SpreadPerName $Reports { param($report) $report.memory.gpu.render_asset_counts }
            unavailable = Get-UnavailableMap $Reports { param($report) $report.memory.gpu.unavailable }
        }
    }
    $window.scene = [ordered]@{
        measured_frames = Get-Spread (Get-Values $Reports { param($report) $report.scene.measured_frames })
        entity_count = Get-Spread (Get-Values $Reports { param($report) $report.scene.entity_count })
        mesh_entities = Get-Spread (Get-Values $Reports { param($report) $report.scene.mesh_entities })
        terrain_patches = Get-Spread (Get-Values $Reports { param($report) $report.scene.terrain_patches })
        water_surfaces = Get-Spread (Get-Values $Reports { param($report) $report.scene.water_surfaces })
        pipeline_statistics_supported = $first.scene.pipeline_statistics_supported
        vertex_shader_invocations = Get-SummaryMap $Reports { param($report) $report.scene.vertex_shader_invocations }
        clipper_primitives_out = Get-SummaryMap $Reports { param($report) $report.scene.clipper_primitives_out }
        draw_calls = Get-Spread (Get-Values $Reports { param($report) $report.scene.draw_calls })
        unavailable = Get-UnavailableMap $Reports { param($report) $report.scene.unavailable }
    }
    $window.cpu = [ordered]@{
        schedules = Get-SummaryMap $Reports { param($report) $report.cpu.schedules }
        spans = Get-SummaryMap $Reports { param($report) $report.cpu.spans }
        render_world_prep = Get-Spread (Get-Values $Reports { param($report) $report.cpu.render_world_prep })
        unavailable = Get-UnavailableMap $Reports { param($report) $report.cpu.unavailable }
    }
    return $window
}

# The same spread for a `name -> number` map (the GPU-asset counts), over the union of the repeats'
# names.
function Get-SpreadPerName($Reports, [scriptblock]$Selector) {
    # The inner scriptblock below must call the *caller's* selector. Inside it, `$Selector` would
    # resolve to the scriptblock itself (a scriptblock closes over the scope that made it), so the
    # selector is copied to a name the closure cannot confuse with itself.
    $select = $Selector
    $names = [ordered]@{}
    foreach ($report in $Reports) {
        $map = & $select $report
        if ($map) { foreach ($key in @($map.Keys)) { $names[$key] = $true } }
    }
    $spread = [ordered]@{}
    foreach ($name in @($names.Keys)) {
        $spread[$name] = Get-Spread (Get-Values $Reports { param($report) (& $select $report)[$name] })
    }
    return $spread
}

function Format-Spread($Spread, [string]$Format = "{0:N3}") {
    if ($null -eq $Spread) { return "n/a" }
    # The coverage follows the range: how many of the window's repeats measured this value.
    return "$([string]::Format($Format, $Spread.median)) ($([string]::Format($Format, $Spread.min))-$([string]::Format($Format, $Spread.max)), $($Spread.samples)/$($Spread.runs) samples)"
}

# A byte spread in MiB, or `n/a` when the repeats did not measure it.
function Format-MiB($Spread) {
    if ($null -eq $Spread) { return "n/a" }
    $mib = @{ median = $Spread.median / 1MB; min = $Spread.min / 1MB; max = $Spread.max / 1MB; samples = $Spread.samples; runs = $Spread.runs }
    return (Format-Spread $mib '{0:N2}')
}

# The reason a section could not measure something, preferring the key the caller is printing, with
# the other keys of the same section listed after it so nothing is dropped.
function Get-UnavailableReason($Map, [string[]]$Keys) {
    if ($null -eq $Map -or $Map.Count -eq 0) { return "no reason recorded" }
    $preferred = foreach ($key in $Keys) { if ($Map.Contains($key) -and $Map[$key]) { "$key`: $($Map[$key])" } }
    if ($preferred) { return ($preferred -join "; ") }
    return (@($Map.Keys | ForEach-Object { "$_`: $($Map[$_])" }) -join "; ")
}

# Every counter a window could not measure, as `section.key = reason` items, so the report explains
# its own gaps instead of leaving a bare `n/a`.
function Get-UnavailableItems($Window) {
    $items = @()
    foreach ($section in @("gpu_passes", "scene", "cpu")) {
        $map = $Window[$section].unavailable
        foreach ($key in @($map.Keys)) { $items += "$section.$key = $($map[$key])" }
    }
    $map = $Window.memory.gpu.unavailable
    foreach ($key in @($map.Keys)) { $items += "memory.gpu.$key = $($map[$key])" }
    return $items
}

# The passes whose own sample count is below the window's measured frames, with frame and repeat coverage,
# with the frame count they were set against, or `$null` when every pass was sampled as often as the
# window had frames (or there is nothing to compare). The engine's `count` is the pass's real sample
# count, so a smaller one means the GPU readback lagged or the pass did not run every frame.
function Get-LaggingPasses($Window) {
    $measured = if ($null -ne $Window.gpu_passes.measured_frames) { $Window.gpu_passes.measured_frames.median } else { $null }
    if ($null -eq $measured -or $measured -le 0) { return $null }
    $passes = @($Window.gpu_passes.passes.Keys | Where-Object {
            $samples = $Window.gpu_passes.pass_samples[$_]
            # `$null -lt $measured` is true in PowerShell, so the entry and its median are both
            # checked before the comparison.
            $null -ne $samples -and $null -ne $samples.median -and $samples.median -lt $measured
        } | Sort-Object)
    if ($passes.Count -eq 0) { return $null }
    return [ordered]@{
        frames = [string]::Format('{0:N0}', $measured)
        passes = @($passes | ForEach-Object {
                "$_ ($(Format-Spread $Window.gpu_passes.pass_samples[$_] '{0:N0}'))"
            })
    }
}

# One short table per scenario window, the window's memory, and the facts a table cannot hold: which
# counters were unavailable and why, and which passes and spans cost the most.
function Get-MarkdownTable($Name, $Window, [string]$Kind) {
    $label = if ($Window.route) { "$Name (route)" } elseif ($Kind -eq "streaming") { "$Name (streaming)" } else { "$Name (standing still)" }
    $lines = @(
        "",
        "## $label",
        "",
        "| Metric | Median (min-max over $($Window.runs) runs) |",
        "|---|---:|",
        "| Average FPS | $(Format-Spread $Window.average_fps '{0:N2}') |",
        "| Mean frame ms | $(Format-Spread $Window.mean_ms) |",
        "| P50 frame ms | $(Format-Spread $Window.p50_ms) |",
        "| P95 frame ms | $(Format-Spread $Window.p95_ms) |",
        "| P99 frame ms | $(Format-Spread $Window.p99_ms) |",
        "| Worst frame ms | $(Format-Spread $Window.worst_ms) |",
        "| Frames measured | $(Format-Spread $Window.count '{0:N0}') |",
        "| Peak process GiB | $(Format-Spread $Window.memory.process_peak_gib) |",
        "| Memory growth GiB | $(Format-Spread $Window.memory.process_growth_gib) |",
        "| Entities | $(Format-Spread $Window.scene.entity_count '{0:N0}') |",
        "| Mesh entities | $(Format-Spread $Window.scene.mesh_entities '{0:N0}') |",
        "| Terrain patches | $(Format-Spread $Window.scene.terrain_patches '{0:N0}') |",
        "| Water surfaces | $(Format-Spread $Window.scene.water_surfaces '{0:N0}') |"
    )
    $budget = $Window.frame_budget
    if ($null -eq $budget) {
        $lines += "| Frame budget | n/a: the report carries no frame_budget section |"
    } else {
        $owners = $budget.over_budget_owners
        $ownerText = (@("main_world", "render_thread", "gpu", "upload", "unknown") | ForEach-Object {
                "$_ $(if ($null -eq $owners[$_]) { 'n/a' } else { [string]::Format('{0:N0}', $owners[$_].median) })"
            }) -join "; "
        $lines += @(
            "| Frame budget ms (scored frames) | $($budget.budget_ms) ($(Format-Spread $budget.frames '{0:N0}') frames) |",
            "| Budget P50 / P99 / P99.9 frame ms | $(Format-Spread $budget.p50_ms) / $(Format-Spread $budget.p99_ms) / $(Format-Spread $budget.p99_9_ms) |",
            "| Budget worst frame ms | $(Format-Spread $budget.worst_ms) |",
            "| Frames over budget | $(Format-Spread $budget.frames_over_budget '{0:N0}') (share within budget $(Format-Spread $budget.share_within_budget '{0:P3}')) |",
            "| Over-budget frames per minute | $(Format-Spread $budget.over_budget_per_minute '{0:N1}') |",
            "| Frames over twice the budget | $(Format-Spread $budget.frames_over_double_budget '{0:N0}') |",
            "| Runs meeting the bar (99.9% within budget, none over twice) | $($budget.runs_meeting_pass_bar) of $($Window.runs) |",
            "| Over-budget frames by owner (median per run) | $ownerText |"
        )
    }
    $route = $Window.route
    if ($route) {
        $lines += @(
            "| Route | $($route.file): $($route.runs_completed) of $($Window.runs) runs completed it ($([string]::Format('{0:N0}', $route.length_units)) units at $($route.speed_units_per_second) units/s); started after $(Format-Spread $route.started_at_run_ms '{0:N0}') ms |",
            "| Route frames without ground | $(Format-Spread $route.route_frames_without_ground '{0:N0}') of $(Format-Spread $route.frames_on_route '{0:N0}') |"
        )
    }
    $lines += "| Draw calls | null: $($Window.scene.unavailable.draw_calls) |"
    if ($Window.scene.pipeline_statistics_supported) {
        $lines += "| Vertex shader invocations | $(Get-SummaryLine $Window.scene.vertex_shader_invocations) |"
        $lines += "| Clipper primitives out | $(Get-SummaryLine $Window.scene.clipper_primitives_out) |"
        $lines += "| Measured frames (pipeline statistics) | $(Format-Spread $Window.scene.measured_frames '{0:N0}') |"
    } else {
        $lines += "| Pipeline-statistics counters | null: $($Window.scene.unavailable.pipeline_statistics) |"
    }
    foreach ($schedule in @("main_schedule/Update", "main_schedule/PostUpdate")) {
        if ($Window.cpu.schedules.Contains($schedule)) {
            $lines += "| $schedule mean ms | $(Format-Spread $Window.cpu.schedules[$schedule]) |"
        }
    }
    foreach ($span in @("streaming/plan_cells", "streaming/cell_commit", "streaming/frame_commit", "streaming/arm_models")) {
        if ($Window.cpu.spans.Contains($span)) {
            $lines += "| $span mean ms | $(Format-Spread $Window.cpu.spans[$span]) |"
        }
    }
    $lines += "| CPU spans recorded | $($Window.cpu.spans.Count) (schedules: $($Window.cpu.schedules.Count)) |"
    $lines += "| Measured frames (GPU passes) | $(Format-Spread $Window.gpu_passes.measured_frames '{0:N0}') |"
    $topPasses = @($Window.gpu_passes.passes.Keys | Sort-Object { -($Window.gpu_passes.passes[$_].median) } | Select-Object -First 5)
    if ($topPasses.Count -eq 0) {
        # The reason GPU pass times are missing is the GPU one; the CPU reason belongs to the CPU
        # schedules and would explain the wrong section.
        $lines += "| Slowest GPU passes | n/a: $(Get-UnavailableReason $Window.gpu_passes.unavailable @('elapsed_gpu', 'passes')) |"
    } else {
        $lines += "| Slowest GPU passes (median ms) | $(($topPasses | ForEach-Object { "$_ $(Format-Spread $Window.gpu_passes.passes[$_])" }) -join '; ') |"
    }
    $topSpans = @($Window.cpu.spans.Keys | Sort-Object { -($Window.cpu.spans[$_].median) } | Select-Object -First 5)
    if ($topSpans.Count -gt 0) {
        $lines += "| Slowest CPU spans (median ms) | $(($topSpans | ForEach-Object { "$_ $(Format-Spread $Window.cpu.spans[$_])" }) -join '; ') |"
    }
    # A pass whose own sample count is below the window's measured frames was not sampled on every
    # frame: its GPU readback lagged, or it did not run every frame. Every pass is listed, not just
    # the slowest, so a counter dropping out of the window is visible.
    $lagging = Get-LaggingPasses $Window
    if ($lagging) {
        $lines += @(
            "",
            "Passes with fewer samples than the $($lagging.frames) frames the window measured: $($lagging.passes -join ', ') - fewer samples than frames (GPU readback lag or a pass that did not run every frame)"
        )
    }
    # Under the window's table, the gates the engine's own verdict failed it on: the numbers above are
    # the measurements of runs that failed a gate, so a reader has to see which gate and how many runs.
    $acceptance = $Window.acceptance
    if ($acceptance -and $acceptance.runs_failed_gates -gt 0) {
        # A failed run with no boolean threshold recorded is possible in principle (a numeric threshold
        # that is not neutralised); naming nothing is clearer than an empty list of names.
        $names = if (@($acceptance.failed_gates).Count -gt 0) { @($acceptance.failed_gates) -join ', ' } else { "no named threshold failed" }
        $lines += @(
            "",
            "Acceptance gates failed in $($acceptance.runs_failed_gates) of $($acceptance.runs) runs: $names. The numbers above are still measurements of those runs."
        )
    }
    # A short resource table: byte figures in MiB, GPU counters as counts with their unit in the row
    # label, so a quick scan cannot mistake one for the other.
    $lines += @(
        "",
        "### Memory",
        "",
        "| Counter | Median (min-max over $($Window.runs) runs) |",
        "|---|---:|",
        "| CPU mesh data (MiB) | $(Format-MiB $Window.memory.cpu.mesh_bytes) |",
        "| CPU image data (MiB) | $(Format-MiB $Window.memory.cpu.image_bytes) |",
        "| Measured frames (GPU counters) | $(Format-Spread $Window.memory.gpu.measured_frames '{0:N0}') |",
        "| GPU mesh slab bytes (MiB) | $(Format-MiB $Window.memory.gpu.mesh_allocator_slab_bytes) |",
        "| GPU mesh allocator slabs (count) | $(Format-Spread $Window.memory.gpu.mesh_allocator_slabs '{0:N0}') |",
        "| GPU mesh allocations (count) | $(Format-Spread $Window.memory.gpu.mesh_allocator_allocations '{0:N0}') |"
    )
    foreach ($asset in @($Window.memory.gpu.render_asset_counts.Keys)) {
        $lines += "| GPU live $asset assets (count) | $(Format-Spread $Window.memory.gpu.render_asset_counts[$asset] '{0:N0}') |"
    }
    $unavailable = @(Get-UnavailableItems $Window)
    if ($unavailable.Count -gt 0) {
        $lines += @("", "Unavailable: $($unavailable -join '; ')")
    }
    return $lines
}

# The largest pass in a per-pass counter map, as one table cell.
function Get-SummaryLine($Summary) {
    if ($null -eq $Summary -or $Summary.Count -eq 0) { return "null: no counter recorded" }
    $top = @($Summary.Keys | Sort-Object { -($Summary[$_].median) } | Select-Object -First 3)
    return ($top | ForEach-Object { "$_ $(Format-Spread $Summary[$_])" }) -join '; '
}

# The campaign format this script writes and reads. Per-window fingerprints and scenario inputs
# arrived in format 2, and the workload fields the comparison checks in scenario_inputs — the flight
# speed, the synthetic instance count and the resolved asset set — in format 3. An older file neither
# carries those fields nor the `measured_frames` a window now keeps, so it cannot be checked against
# this run: a comparison with it is refused rather than made on fields it does not carry. The data
# set's own identity (`assets_identity`, and the reason beside it) was added to that same map without
# a version change: a format-3 file written before it carries no such field, and the window comparison
# refuses it as a missing input rather than comparing numbers whose data set it cannot name.
$script:baselineFormatVersion = 3

# Why a file can never be compared with this run, as a readable reason, or `$null` when its format
# and provenance are the ones a comparison needs. It is checked before the first timed window, so a
# stale file in the default -Out costs nothing and an explicit -CompareTo is refused before the
# measured windows are spent on a comparison that cannot be made.
function Get-BaselineProblem($Baseline) {
    if ($null -eq $Baseline -or $Baseline -isnot [System.Collections.IDictionary]) { return "it is not a JSON object" }
    $version = if ($Baseline.Contains("format_version")) { $Baseline.format_version } else { $null }
    if ($version -ne $script:baselineFormatVersion) {
        return "it is baseline format $(Format-FieldValue $version), and this script reads format $($script:baselineFormatVersion) (scenario inputs that name the flight speed, synthetic instance count and asset set, and per-window measured_frames); rerun the older baseline with this script to compare against it"
    }
    if ($null -eq $Baseline.fingerprint) { return "it recorded no run fingerprint, so the machine and build it ran on cannot be checked" }
    if ($null -eq $Baseline.scenarios) { return "it carries no scenarios map, so it holds no window to compare with" }
    foreach ($scenario in @($Baseline.scenarios.Values)) {
        foreach ($window in @($scenario.windows.Values)) {
            $shortfall = Get-RouteShortfall $window
            if ($shortfall) { return "it holds a partial route window ($shortfall), which is not a comparable full-route workload" }
        }
    }
    return $null
}

# Refuses a comparison the fields say cannot be made: two runs on a different machine, build,
# resolution, driver, renderer or world-data schema. A different commit is expected — comparing
# across commits is what the baseline is for — but everything else that changes the numbers must be
# the same on both sides.
#
# The hardware label is checked here as well as the fingerprint's renderer fields: the label holds the
# CPU as well as the GPU and its driver, and the fingerprint does not carry the CPU, so two machines
# with the same adapter and driver would otherwise compare as one.
function Assert-Comparable($Old, $NewFingerprint, [string]$Path, [string]$NewHardware) {
    $problem = Get-BaselineProblem $Old
    if ($problem) { throw "Cannot compare with ${Path}: $problem" }
    if ([string]$Old.hardware -ne [string]$NewHardware) {
        throw "Cannot compare with ${Path}: the two campaigns' hardware labels differ ($(Format-FieldValue $Old.hardware) -> $(Format-FieldValue $NewHardware)). The label names the CPU, GPU and driver the numbers were measured on, and streaming timings are CPU-sensitive; rerun the baseline on this machine to compare against it"
    }
    $oldInvariant = Get-FingerprintInvariant $Old.fingerprint
    $newInvariant = Get-FingerprintInvariant $NewFingerprint
    if ($null -eq $oldInvariant -or $null -eq $newInvariant) {
        throw "Cannot compare with ${Path}: one of the two baselines recorded no run fingerprint, so the machine and build they ran on cannot be checked"
    }
    $differences = @(Get-FieldDifferences $oldInvariant $newInvariant)
    if ($differences.Count -gt 0) {
        throw "Cannot compare with ${Path}: the machine, build, resolution, driver or schema fields differ ($($differences -join '; ')). Engine results compare only on the same resolution, build profile, driver, backend, adapter and schemas; only the commit is expected to differ"
    }
}

# A missing identity cannot establish an asset-backed comparison, even when the paths match.
function Assert-AssetIdentityKnown($Inputs, [string]$Path, [string]$Scope) {
    if ($Inputs.assets -and $Inputs.assets_identity_unavailable) {
        throw "Cannot compare $Scope with ${Path}: the data set at $($Inputs.assets) has no known identity ($($Inputs.assets_identity_unavailable)). A comparison needs both data sets identified; rerun the window on a data set whose conversion manifest records its configuration hash and schema version"
    }
}

# The planned workload of every window of the campaign, compared with the baseline window of the same
# name and kind before the first engine starts. Only what is known before the run is compared: the
# stream radius and warm-up are report fields and are checked against the report after its window.
# A route window's identity is its file's hash, so an edited route, its coordinates, speed or
# worldspace, is a different workload. The frame budget is part of the workload too.
function Assert-ComparisonWorkloads($Old, $Planned, [double]$FrameBudgetMs, [string]$Path) {
    foreach ($key in @($Planned.Keys)) {
        $parts = $key -split "/"
        $name = $parts[0]; $kind = $parts[1]
        if (-not $Old.scenarios.ContainsKey($name)) { continue }
        $oldWindow = $Old.scenarios[$name].windows[$kind]
        if ($null -eq $oldWindow) { continue }
        $planWindow = $Planned[$key]
        Assert-AssetIdentityKnown $planWindow $Path "baseline $key"
        $oldInputs = [ordered]@{}
        foreach ($field in $script:knownInputKeys) { $oldInputs[$field] = $oldWindow.scenario_inputs[$field] }
        $newInputs = [ordered]@{}
        foreach ($field in $script:knownInputKeys) { $newInputs[$field] = $planWindow[$field] }
        $differences = @(Get-FieldDifferences $oldInputs $newInputs)
        if ($null -ne $oldWindow.frame_budget -and [double]$oldWindow.frame_budget.budget_ms -ne $FrameBudgetMs) {
            $differences += "frame_budget_ms : $(Format-FieldValue $oldWindow.frame_budget.budget_ms) -> $(Format-FieldValue $FrameBudgetMs)"
        }
        if ($differences.Count -gt 0) {
            throw "Cannot compare baseline $key with ${Path}: the window would not be the same workload ($($differences -join '; ')). The check runs before the campaign's first timed window, so nothing was measured; rerun without -CompareTo, or with a baseline measured on this workload"
        }
    }
}

# Asset identity needs no engine report, so refuse unusable comparisons before timed windows.
function Assert-ComparisonAssetIdentities($Old, [string]$ResolvedAssets, $Requested, $WindowCounts, [string]$Path) {
    foreach ($name in $Requested) {
        if ($name -eq "synthetic" -or -not $Old.scenarios.ContainsKey($name)) { continue }
        $kinds = if ($WindowCounts[$name] -eq 2) { @("standing_still", "streaming") } else { @("standing_still") }
        foreach ($kind in $kinds) {
            $oldWindow = $Old.scenarios[$name].windows[$kind]
            if ($null -eq $oldWindow) { continue }
            Assert-AssetIdentityKnown $oldWindow.scenario_inputs $Path "baseline $name/$kind"
            $identity = Get-AssetsIdentity $ResolvedAssets
            Assert-AssetIdentityKnown @{ assets = $ResolvedAssets; assets_identity_unavailable = $identity.unavailable } $Path "baseline $name/$kind"
        }
    }
}

# Refuses a comparison between two windows that are not the same workload: the scenario inputs
# (worldspace, grid, flight speed, synthetic instance count, asset path and its conversion-manifest
# identity, stream radius, duration, warm-up), the frame budget the frames were counted over, the
# effective graphics settings and the world database opened must match.
function Assert-WindowComparable($OldWindow, $NewWindow, [string]$Path, [string]$Scope) {
    # Two unknown identities are not the same workload, even when they name one path and give the
    # same reason, so an asset-backed window with no readable identity on either side is refused.
    # A window with no assets (the synthetic scene) has no data set to identify and is not affected.
    foreach ($side in @($OldWindow, $NewWindow)) {
        Assert-AssetIdentityKnown $side.scenario_inputs $Path $Scope
        $shortfall = Get-RouteShortfall $side
        if ($shortfall) { throw "Cannot compare $Scope with ${Path}: a partial route window ($shortfall) is not a full-route workload" }
    }
    $differences = @(Get-FieldDifferences $OldWindow.scenario_inputs $NewWindow.scenario_inputs)
    # The frame budget is workload too — the report counts frames over it — and it is a number of the
    # window rather than of `scenario_inputs`. A window from before the budget was recorded carries
    # none, and still compares.
    if ($null -ne $OldWindow.frame_budget -and $null -ne $NewWindow.frame_budget) {
        $differences += Get-FieldDifferences ([ordered]@{ frame_budget_ms = $OldWindow.frame_budget.budget_ms }) ([ordered]@{ frame_budget_ms = $NewWindow.frame_budget.budget_ms })
    }
    $differences += Get-FieldDifferences $OldWindow.fingerprint.graphics $NewWindow.fingerprint.graphics "graphics"
    $oldSchema = $OldWindow.fingerprint.opened_world_database_schema
    $newSchema = $NewWindow.fingerprint.opened_world_database_schema
    if ((Format-FieldValue $oldSchema) -ne (Format-FieldValue $newSchema)) {
        $differences += "opened_world_database_schema : $(Format-FieldValue $oldSchema) -> $(Format-FieldValue $newSchema)"
    }
    if ($differences.Count -gt 0) {
        throw "Cannot compare $Scope with ${Path}: the two windows are not the same workload ($($differences -join '; ')). A comparison needs the same scenario inputs, frame budget, graphics settings and schemas; only the commit may differ"
    }
}

# The refusal message of a comparison check, or `$null` when the check passed. A comparison the user
# asked for with -CompareTo is refused by throwing; a baseline picked up from -Out by default was
# never asked for, so its refusal is returned for the caller to warn about and drop instead of
# aborting a campaign that does not depend on the comparison.
function Get-ComparisonRefusal([scriptblock]$Check) {
    $refusal = $null
    try { & $Check } catch { $refusal = $_.Exception.Message }
    if (-not $refusal) { return $null }
    if ($explicitCompare) { throw $refusal }
    return $refusal
}

# Per-metric comparison against an older baseline of the same machine and backend, reusing the
# profiling campaign's noise floors. A metric is scored only when both sides measured it in every
# repeat: the median of a partial set is a different sample from a full one, so it is listed under
# `unscored` with its coverage and the reason, rather than compared.
function Get-Comparison($Old, $New) {
    $comparison = [ordered]@{}
    $unscored = [ordered]@{}
    foreach ($name in @($New.scenarios.Keys)) {
        if (-not $Old.scenarios.ContainsKey($name)) { continue }
        foreach ($kind in @("standing_still", "streaming")) {
            $newWindow = $New.scenarios[$name].windows[$kind]
            $oldWindow = $Old.scenarios[$name].windows[$kind]
            if ($null -eq $newWindow -or $null -eq $oldWindow) { continue }
            $metrics = [ordered]@{
                average_fps = @($newWindow.average_fps, $oldWindow.average_fps)
                frame_ms_p95 = @($newWindow.p95_ms, $oldWindow.p95_ms)
                frame_ms_p99 = @($newWindow.p99_ms, $oldWindow.p99_ms)
                peak_process_memory_gib = @($newWindow.memory.process_peak_gib, $oldWindow.memory.process_peak_gib)
                process_memory_growth_gib = @($newWindow.memory.process_growth_gib, $oldWindow.memory.process_growth_gib)
            }
            $current = @{}
            $skipped = [ordered]@{}
            foreach ($metric in @($metrics.Keys)) {
                # Named `$newSpread`/`$oldSpread`, not `$new`/`$old`: variable names are
                # case-insensitive, so `$new = ...` would overwrite the `$New` parameter and the next
                # window would be read out of a metric spread.
                $newSpread = $metrics[$metric][0]; $oldSpread = $metrics[$metric][1]
                if ($null -eq $newSpread -or $null -eq $oldSpread -or $oldSpread.median -le 0) { continue }
                if ($newSpread.samples -ne $New.repeats -or $oldSpread.samples -ne $Old.repeats) {
                    $skipped[$metric] = [ordered]@{
                        baseline = $oldSpread.median; current = $newSpread.median
                        baseline_samples = $oldSpread.samples; baseline_repeats = $Old.repeats
                        current_samples = $newSpread.samples; current_repeats = $New.repeats
                        reason = "measured in $($newSpread.samples) of $($New.repeats) current repeats and $($oldSpread.samples) of $($Old.repeats) baseline repeats; a partial median is not compared"
                    }
                    continue
                }
                $regression = Get-RegressionComparison -Metric $metric -Old $oldSpread.median -New $newSpread.median
                $current["$name/$kind/$metric"] = [ordered]@{
                    baseline = $oldSpread.median; current = $newSpread.median
                    regression_percent = $regression.regression_percent
                    regression_absolute = $regression.regression_absolute
                    noise_floor = $regression.noise_floor
                    status = $regression.status
                }
            }
            if ($current.Count -gt 0) { $comparison["$name/$kind"] = $current }
            if ($skipped.Count -gt 0) { $unscored["$name/$kind"] = $skipped }
        }
    }
    return [ordered]@{ metrics = $comparison; unscored = $unscored }
}

$windowsPerScenario = @{}
foreach ($name in $requested) {
    $moving = $false
    if ($name -ne "synthetic") {
        $probe = Get-WindowArguments $name @{
            RunDirectory = "."; Commit = "probe"; Hardware = "probe"; DurationSeconds = 0; ResolvedAssets = $resolvedAssets
        }
        $moving = ($probe -contains "--auto-fly-speed")
    }
    # A flying scenario gets two windows (still, then flying), a route scenario only the moving one.
    $windowsPerScenario[$name] = if ($routeScenarios.ContainsKey($name)) { 1 } elseif ($moving) { 2 } else { 1 }
}
$totalWindows = ($requested | ForEach-Object { $windowsPerScenario[$_] * $Repeats } | Measure-Object -Sum).Sum
$totalSeconds = ($requested | ForEach-Object { $windowsPerScenario[$_] * $Repeats * (Get-WindowDuration $_) } | Measure-Object -Sum).Sum
$plan = ($requested | ForEach-Object {
        "$_ x $($windowsPerScenario[$_]) window(s) x $(Get-WindowDuration $_) s"
    }) -join ", "
Write-Host "Baseline plan: $Repeats repeat(s) of $plan"
Write-Host "Expected duration: $totalWindows engine window(s), about $( [Math]::Round($totalSeconds / 60.0, 1) ) minutes of measured time, plus the build"

# The windows are timed, so the runner itself must not be scheduled below the machine's other work
# (the profiling campaign sets the same priority).
$benchmarkPriority = [Diagnostics.ProcessPriorityClass]::AboveNormal
try {
    (Get-Process -Id $PID).PriorityClass = $benchmarkPriority
} catch {
    throw "Could not set the baseline runner priority to $benchmarkPriority`: $($_.Exception.Message)"
}

if (-not $NoBuild -or -not (Test-Path -LiteralPath $engine -PathType Leaf)) {
    Invoke-EngineReleaseBuild
}
$identity = Get-BenchmarkRunIdentity
$hardware = Get-BenchmarkHardware
# Every campaign's report bundles go to their own timestamped directory, so a `run-N` this campaign
# reads back can never be a file an earlier campaign left behind.
$stamp = Get-Date -Format "yyyyMMdd-HHmmss"
$campaignRoot = Join-Path $Out "runs\$stamp-$hardware"

$comparePath = if ($CompareTo) {
    if ([IO.Path]::IsPathRooted($CompareTo)) { $CompareTo } else { Join-Path $repository $CompareTo }
} else {
    Join-Path $Out "baseline.json"
}
# An explicitly requested comparison against a file that is not there is a mistyped path, not a run
# without a previous baseline, so it is refused rather than silently treated as no comparison.
if ($CompareTo -and -not (Test-Path -LiteralPath $comparePath -PathType Leaf)) {
    throw "Cannot compare: -CompareTo $comparePath does not exist"
}
# `-CompareTo` names a comparison the user asked for, so a file that cannot be used is refused. A
# `baseline.json` that merely happens to sit in -Out is picked up by default, and that file may be
# from an older format, half-written, or measured on another machine: it is checked here, before the
# first timed window, and dropped with one warning rather than aborting a campaign that never asked
# to compare.
$explicitCompare = [bool]$CompareTo
$previous = $null
if (Test-Path -LiteralPath $comparePath -PathType Leaf) {
    $candidate = $null
    $problem = $null
    try {
        $candidate = Get-Content -LiteralPath $comparePath -Raw | ConvertFrom-Json -AsHashtable
        $problem = Get-BaselineProblem $candidate
    } catch {
        $candidate = $null
        $problem = "it is not readable JSON ($($_.Exception.Message))"
    }
    if ($problem) {
        if ($explicitCompare) { throw "Cannot compare with ${comparePath}: $problem" }
        Write-Warning "Not comparing with ${comparePath}: $problem. The campaign runs without a comparison; pass -CompareTo to have a baseline like this refused instead"
    } else {
        $previous = $candidate
    }
}

if ($previous) {
    $refusal = Get-ComparisonRefusal { Assert-ComparisonAssetIdentities $previous $resolvedAssets $requested $windowsPerScenario $comparePath }
    if ($refusal) {
        Write-Warning "$refusal The campaign continues without a comparison"
        $previous = $null
    }
}
if ($previous) {
    $planned = Get-PlannedWorkloads $requested $windowsPerScenario
    $refusal = Get-ComparisonRefusal { Assert-ComparisonWorkloads $previous $planned $FrameBudgetMs $comparePath }
    if ($refusal) {
        Write-Warning "$refusal The campaign continues without a comparison"
        $previous = $null
    }
}

# Named `$scenarioReports`, not `$scenarios`: PowerShell variable names are case-insensitive, so a
# local `$scenarios` would be this script's own `-Scenarios` parameter, validation and all.
$scenarioReports = [ordered]@{}
# The machine/build invariants of the campaign, taken from the first window, and the label of the
# window they came from so a later window that disagrees can name both.
$campaignFingerprint = $null
$campaignFingerprintLabel = $null
foreach ($name in $requested) {
    $isRoute = $routeScenarios.ContainsKey($name)
    $moves = $isRoute -or $windowsPerScenario[$name] -eq 2
    $kinds = Get-WindowKinds $name $windowsPerScenario
    $reports = @{}
    foreach ($kind in $kinds) {
        $windowReports = @()
        $windowArguments = $null
        for ($run = 1; $run -le $Repeats; $run++) {
            # A moving scenario gets two windows: the scene left standing still (the steady frames),
            # then the scenario's own flight (the streaming frames). Everything else is one window.
            $label = if ($kind -eq "streaming") { "baseline $name streaming $run/$Repeats" } else { "baseline $name $run/$Repeats" }
            $reportLabel = "baseline $name/$kind/run-$run"
            $runDirectory = Join-Path $campaignRoot "$name\$($kind -replace '_', '-')\run-$run"
            $arguments = Get-WindowArguments $name (Get-WindowInvocationParameters $name $kind @{
                RunDirectory = $runDirectory; Commit = $identity.commit
                Hardware = $hardware; RunId = "run-$run"; RunLabel = $label; DirtyWorktree = [bool]$identity.dirty
            })
            if ($null -eq $windowArguments) { $windowArguments = $arguments }
            # A baseline measures a window; it is not the acceptance campaign, so a window the engine
            # ended on a failed gate is kept with its gate recorded (`acceptance`, below) rather than
            # thrown away. Every check that says the report is this repeat's still applies.
            $report = Invoke-EngineWindow -Engine $engine -Arguments $arguments -RunDirectory $runDirectory `
                -Label "Baseline run $name/$kind/run-$run" -AsHashtable -AllowGateFailure
            if ($null -eq $report.fingerprint) {
                throw "$reportLabel recorded no run fingerprint in $runDirectory, so the window cannot be shown to be comparable with the others"
            }
            if ($null -eq $campaignFingerprint) {
                $campaignFingerprint = Get-FingerprintInvariant $report.fingerprint
                $campaignFingerprintLabel = $reportLabel
                # Comparing is only meaningful between the same machine, build and schemas, so a
                # mismatch is refused before the rest of the campaign spends hours producing numbers
                # nobody can set against the older baseline — unless the baseline was picked up from
                # -Out, in which case it is dropped with one warning and the campaign goes on.
                if ($previous) {
                    $refusal = Get-ComparisonRefusal { Assert-Comparable -Old $previous -NewFingerprint $campaignFingerprint -Path $comparePath -NewHardware $hardware }
                    if ($refusal) {
                        Write-Warning "$refusal The campaign continues without a comparison"
                        $previous = $null
                    }
                }
            } else {
                # Every window after the first must agree with it on the machine, the build and the
                # schemas, or the campaign is not one comparable set of numbers.
                $differences = @(Get-FieldDifferences $campaignFingerprint (Get-FingerprintInvariant $report.fingerprint))
                if ($differences.Count -gt 0) {
                    throw "The windows of this campaign were not measured on the same machine and build: $reportLabel differs from ${campaignFingerprintLabel} in $($differences -join '; '). A baseline mixes only windows measured on one machine, backend and build"
                }
            }
            $windowReports += $report
        }
        $inputs = Get-ScenarioInputs -Arguments $windowArguments -Report $windowReports[0] -DurationSeconds (Get-WindowDuration $name)
        $reports[$kind] = Get-WindowReport $windowReports $kind $inputs
        # The workload itself must match the older baseline's window before any number in it is
        # compared, so a rerun with different coordinates, flight speed, asset set or settings is
        # refused, not scored — refused outright only when the baseline was asked for by -CompareTo.
        if ($previous -and $previous.scenarios.ContainsKey($name)) {
            $oldWindow = $previous.scenarios[$name].windows[$kind]
            if ($oldWindow) {
                $refusal = Get-ComparisonRefusal { Assert-WindowComparable -OldWindow $oldWindow -NewWindow $reports[$kind] -Path $comparePath -Scope "baseline $name/$kind" }
                if ($refusal) {
                    Write-Warning "$refusal The campaign continues without a comparison"
                    $previous = $null
                }
            }
        }
    }
    $entry = [ordered]@{ moves = $moves; windows = [ordered]@{} }
    foreach ($kind in @("standing_still", "streaming")) {
        $entry.windows[$kind] = if ($reports.ContainsKey($kind)) { $reports[$kind] } else { $null }
    }
    if (-not $moves) {
        $entry.streaming_unavailable = "the $name scenario leaves the camera in place, so it has no streaming window"
    }
    $scenarioReports[$name] = $entry
}

# Named `$baselineReport`, not `$baseline`: the dot-sourced profile script declares a `$Baseline`
# parameter, and PowerShell would treat the two as the same variable.
$baselineReport = [ordered]@{
    format_version = $script:baselineFormatVersion
    generated_at = (Get-Date).ToString("o")
    commit = $identity.commit
    dirty = $identity.dirty
    hardware = $hardware
    repeats = $Repeats
    duration_seconds = $DurationSeconds
    stability_seconds = $StabilitySeconds
    frame_budget_ms = $FrameBudgetMs
    # Where this campaign's own report bundles are, so a window's raw evidence can be found again
    # after a later campaign has written to the same -Out.
    run_directory = $campaignRoot
    fingerprint = $campaignFingerprint
    scenarios = $scenarioReports
}
if ($previous) {
    $comparisons = Get-Comparison -Old $previous -New $baselineReport
    $baselineReport.comparison = [ordered]@{
        against = $comparePath
        against_commit = $previous.commit
        against_generated_at = $previous.generated_at
        metrics = $comparisons.metrics
        unscored = $comparisons.unscored
    }
}
$baselineReport | ConvertTo-Json -Depth 12 | Set-Content -LiteralPath (Join-Path $Out "baseline.json")

$rendererLine = if ($null -eq $campaignFingerprint.renderer) {
    "not recorded: the run installed no renderer fingerprint"
} else {
    "$($campaignFingerprint.renderer.backend) / $($campaignFingerprint.renderer.adapter) ($($campaignFingerprint.renderer.device_type)), driver $($campaignFingerprint.renderer.driver); GPU timestamps: $($campaignFingerprint.renderer.timestamp_queries_supported); pipeline statistics: $($campaignFingerprint.renderer.pipeline_statistics_supported)"
}
$markdown = @(
    "# Phase 2 engine baseline",
    "",
    "- Commit: $($identity.commit) (dirty worktree: $($identity.dirty))",
    "- Machine: $hardware",
    "- Renderer: $rendererLine",
    "- Repeats: $Repeats; time limit: $DurationSeconds s per window ($StabilitySeconds s for stability); a route window runs at least its travel time plus $RouteReadinessSeconds s, so the actual length of a route window is not uniform",
    "- Frame budget: $FrameBudgetMs ms ($([Math]::Round(1000.0 / $FrameBudgetMs, 1)) fps); proposed bar: 99.9% of frames within it, none over twice it",
    "- Generated: $($baselineReport.generated_at)",
    "- Report bundles: $campaignRoot",
    ""
)
foreach ($name in $requested) {
    foreach ($kind in @("standing_still", "streaming")) {
        $window = $scenarioReports[$name].windows[$kind]
        if ($null -eq $window) {
            if ($kind -eq "streaming") { $markdown += @("", "## $name (streaming)", "", $scenarioReports[$name].streaming_unavailable) }
            continue
        }
        $markdown += Get-MarkdownTable $name $window $kind
    }
}
if ($previous) {
    $markdown += @(
        "",
        "## Comparison against $comparePath",
        "",
        "- Compared with commit $($previous.commit), generated $($previous.generated_at); this run is commit $($identity.commit).",
        "- The two runs were checked to share the machine (the hardware label, backend, adapter and driver), resolution, build profile, schemas, graphics settings and scenario inputs — the data set's identity (its world database and cell cache files, and the conversion manifest's settings) among them — and the frame budget; the commit is the field allowed to differ.",
        "",
        "| Window | Metric | Baseline | Current | Change % | Status |",
        "|---|---|---:|---:|---:|---|"
    )
    foreach ($scope in @($baselineReport.comparison.metrics.Keys)) {
        # The scope (`<scenario>/<window>`) is printed with every row: the same metric name appears in
        # every window's rows, and a bare `average_fps` row says nothing about what was measured.
        foreach ($metric in @($baselineReport.comparison.metrics[$scope].Keys)) {
            $value = $baselineReport.comparison.metrics[$scope][$metric]
            $markdown += "| $scope | $metric | $([Math]::Round($value.baseline, 4)) | $([Math]::Round($value.current, 4)) | $([Math]::Round($value.regression_percent, 2)) | $($value.status) |"
        }
    }
    foreach ($scope in @($baselineReport.comparison.unscored.Keys)) {
        foreach ($metric in @($baselineReport.comparison.unscored[$scope].Keys)) {
            $value = $baselineReport.comparison.unscored[$scope][$metric]
            $markdown += "| $scope | $metric | $([Math]::Round($value.baseline, 4)) | $([Math]::Round($value.current, 4)) | n/a | unscored: $($value.reason) |"
        }
    }
}
$markdown | Set-Content -LiteralPath (Join-Path $Out "baseline.md")
Write-Host "Baseline complete: $(Join-Path $Out 'baseline.json') and $(Join-Path $Out 'baseline.md')"
