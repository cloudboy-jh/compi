[CmdletBinding()]
param(
    [string]$BinaryDirectory = (Join-Path $PSScriptRoot '..\target\release'),
    [ValidateSet('warm', 'cold', 'empty')]
    [string[]]$Mode = @('warm', 'cold', 'empty'),
    [ValidateRange(1, 100)]
    [int]$Samples = 10,
    [switch]$ConfirmPhysicalDisplay
)

Set-StrictMode -Version Latest
$ErrorActionPreference = 'Stop'

$clientPath = Join-Path $BinaryDirectory 'compi.exe'
$daemonPath = Join-Path $BinaryDirectory 'compi-daemon.exe'
$probePath = Join-Path $BinaryDirectory 'examples\compi-probe.exe'
foreach ($path in @($clientPath, $daemonPath, $probePath)) {
    if (-not (Test-Path $path -PathType Leaf)) {
        throw "Required measurement binary was not found: $path"
    }
}

$compiData = Join-Path $env:LOCALAPPDATA 'Compi'
$measurementDirectory = Join-Path $compiData 'measurements'
New-Item -ItemType Directory -Path $measurementDirectory -Force | Out-Null
$runId = 'release-{0:yyyyMMdd-HHmmss}-{1}' -f (Get-Date), $PID
$instanceBase = 'm{0:MMddHHmmss}{1}' -f (Get-Date), $PID
$startupLog = Join-Path $compiData 'client-startup.log'
$results = [System.Collections.Generic.List[object]]::new()
$startedProcesses = [System.Collections.Generic.List[System.Diagnostics.Process]]::new()

function Start-WithEnvironment {
    param(
        [Parameter(Mandatory)] [string]$FilePath,
        [string[]]$ArgumentList = @(),
        [Parameter(Mandatory)] [hashtable]$Environment
    )

    $prior = @{}
    try {
        foreach ($entry in $Environment.GetEnumerator()) {
            $prior[$entry.Key] = [Environment]::GetEnvironmentVariable($entry.Key, 'Process')
            [Environment]::SetEnvironmentVariable($entry.Key, [string]$entry.Value, 'Process')
        }
        $startParameters = @{ FilePath = $FilePath; PassThru = $true }
        if ($ArgumentList) {
            $startParameters.ArgumentList = $ArgumentList
        }
        $process = Start-Process @startParameters
        $script:startedProcesses.Add($process)
        return $process
    }
    finally {
        foreach ($entry in $prior.GetEnumerator()) {
            [Environment]::SetEnvironmentVariable($entry.Key, $entry.Value, 'Process')
        }
    }
}

function Wait-StartupMetric {
    param(
        [Parameter(Mandatory)] [string]$Sample,
        [Parameter(Mandatory)] [string]$Metric,
        [int]$TimeoutSeconds = 20
    )

    $deadline = [DateTime]::UtcNow.AddSeconds($TimeoutSeconds)
    $pattern = 'sample={0} .*metric={1} value_ms=(\d+)' -f [regex]::Escape($Sample), [regex]::Escape($Metric)
    while ([DateTime]::UtcNow -lt $deadline) {
        if (Test-Path $startupLog) {
            $match = Get-Content $startupLog | Select-String -Pattern $pattern | Select-Object -Last 1
            if ($match -and $match.Matches[0].Groups[1].Success) {
                return [int64]$match.Matches[0].Groups[1].Value
            }
        }
        Start-Sleep -Milliseconds 50
    }
    throw "Timed out waiting for $Metric in sample $Sample"
}

function Wait-ResourceSample {
    param(
        [Parameter(Mandatory)] [System.Diagnostics.Process]$Process,
        [Parameter(Mandatory)] [string]$ProcessKind,
        [Parameter(Mandatory)] [string]$Sample,
        [Nullable[int]]$ExpectedSessions,
        [int]$TimeoutSeconds = 24
    )

    $path = Join-Path $compiData ("{0}-resource-{1}.log" -f $ProcessKind, $Process.Id)
    $deadline = [DateTime]::UtcNow.AddSeconds($TimeoutSeconds)
    while ([DateTime]::UtcNow -lt $deadline) {
        if (Test-Path $path) {
            $line = Get-Content $path |
                Where-Object {
                    $_ -match "sample=$([regex]::Escape($Sample)) " -and
                    ($null -eq $ExpectedSessions -or $_ -match " sessions=$ExpectedSessions ")
                } |
                Select-Object -Last 1
            if ($line) {
                return $line
            }
        }
        if ($Process.HasExited) {
            throw "$ProcessKind process $($Process.Id) exited before its stabilized resource sample"
        }
        Start-Sleep -Milliseconds 100
    }
    throw "Timed out waiting for stabilized $ProcessKind resources in sample $Sample"
}

function Wait-Daemon {
    param([Parameter(Mandatory)] [string]$Instance)

    $deadline = [DateTime]::UtcNow.AddSeconds(15)
    while ([DateTime]::UtcNow -lt $deadline) {
        & $probePath --instance $Instance list *> $null
        if ($LASTEXITCODE -eq 0) {
            return
        }
        Start-Sleep -Milliseconds 100
    }
    throw "Daemon instance $Instance did not become ready"
}

function Stop-Client {
    param([Parameter(Mandatory)] [System.Diagnostics.Process]$Process)

    if (-not $Process.HasExited) {
        Stop-Process -Id $Process.Id
        $Process.WaitForExit(5000) | Out-Null
    }
}

function Stop-Daemon {
    param([Parameter(Mandatory)] [string]$Instance)

    & $probePath --instance $Instance shutdown *> $null
    if ($LASTEXITCODE -ne 0) {
        throw "Could not stop daemon instance $Instance cleanly"
    }
}

function Get-GpuMemory {
    param([Parameter(Mandatory)] [int]$ProcessId)

    $dedicated = $null
    $shared = $null
    try {
        $sample = Get-Counter @(
            '\GPU Process Memory(*)\Dedicated Usage',
            '\GPU Process Memory(*)\Shared Usage'
        ) -ErrorAction Stop
        $matching = $sample.CounterSamples | Where-Object {
            $_.InstanceName -match "pid_${ProcessId}_"
        }
        $dedicatedValues = @($matching | Where-Object Path -Like '*Dedicated Usage' | Select-Object -ExpandProperty CookedValue)
        $sharedValues = @($matching | Where-Object Path -Like '*Shared Usage' | Select-Object -ExpandProperty CookedValue)
        if ($dedicatedValues.Count -gt 0) {
            $dedicated = [int64](($dedicatedValues | Measure-Object -Sum).Sum)
        }
        if ($sharedValues.Count -gt 0) {
            $shared = [int64](($sharedValues | Measure-Object -Sum).Sum)
        }
    }
    catch {
        Write-Warning "GPU process-memory counters are unavailable: $($_.Exception.Message)"
    }
    return @($dedicated, $shared)
}

function Add-ProcessMeasurement {
    param(
        [Parameter(Mandatory)] [string]$Sample,
        [Parameter(Mandatory)] [string]$Startup,
        [Parameter(Mandatory)] [string]$ProcessKind,
        [Parameter(Mandatory)] [System.Diagnostics.Process]$Process,
        [Nullable[int64]]$FirstWindowMs,
        [Nullable[int64]]$FirstTerminalMs,
        [Nullable[int64]]$ReadyForInputMs,
        [Nullable[int64]]$InputToRenderMs,
        [string]$ResourceLine
    )

    $Process.Refresh()
    $gpu = Get-GpuMemory -ProcessId $Process.Id
    $script:results.Add([pscustomobject]@{
        run_id = $runId
        sample = $Sample
        startup = $Startup
        process_kind = $ProcessKind
        process_id = $Process.Id
        first_window_ms = $FirstWindowMs
        first_terminal_ms = $FirstTerminalMs
        ready_for_input_ms = $ReadyForInputMs
        input_to_render_ms = $InputToRenderMs
        private_bytes = $Process.PrivateMemorySize64
        working_set_bytes = $Process.WorkingSet64
        handles = $Process.HandleCount
        gpu_dedicated_bytes = $gpu[0]
        gpu_shared_bytes = $gpu[1]
        resource_log = $ResourceLine
    })
}

function Get-DaemonProcess {
    param([Parameter(Mandatory)] [string]$Instance)

    $match = Get-CimInstance Win32_Process -Filter "Name = 'compi-daemon.exe'" |
        Where-Object { $_.CommandLine -and $_.CommandLine.Contains("--instance $Instance") }
    if (@($match).Count -ne 1) {
        throw "Expected exactly one daemon process for instance $Instance"
    }
    return Get-Process -Id $match.ProcessId
}

function Invoke-ClientSample {
    param(
        [Parameter(Mandatory)] [string]$Startup,
        [Parameter(Mandatory)] [string]$Sample,
        [string]$Instance,
        [switch]$EmptyWindow,
        [ValidateRange(1, 16)] [int]$SessionCount = 1
    )

    $environment = @{
        COMPI_PERF_LOG = '1'
        COMPI_PERF_SAMPLE = $Sample
        COMPI_PERF_STARTUP_KIND = $Startup
    }
    $environment.COMPI_PERF_SESSION_COUNT = $SessionCount
    if ($EmptyWindow) {
        $environment.COMPI_PERF_EMPTY_WINDOW = '1'
    }
    else {
        $environment.COMPI_PERF_READY_PROBE = '1'
    }
    $arguments = if ($Instance) { @('--instance', $Instance) } else { @() }
    $client = Start-WithEnvironment -FilePath $clientPath -ArgumentList $arguments -Environment $environment
    try {
        $firstWindow = Wait-StartupMetric -Sample $Sample -Metric 'first_window_frame_ms'
        $firstTerminal = $null
        $ready = $null
        $inputToRender = $null
        if (-not $EmptyWindow) {
            $firstTerminal = Wait-StartupMetric -Sample $Sample -Metric 'first_terminal_frame_ms'
            $ready = Wait-StartupMetric -Sample $Sample -Metric 'ready_for_input_ms'
            $inputToRender = Wait-StartupMetric -Sample $Sample -Metric 'input_to_render_ms'
        }
        $expectedSessions = if ($EmptyWindow) { 0 } else { $SessionCount }
        $resource = Wait-ResourceSample -Process $client -ProcessKind 'client' `
            -Sample $Sample -ExpectedSessions $expectedSessions
        Add-ProcessMeasurement -Sample $Sample -Startup $Startup -ProcessKind 'client' `
            -Process $client -FirstWindowMs $firstWindow -FirstTerminalMs $firstTerminal `
            -ReadyForInputMs $ready -InputToRenderMs $inputToRender `
            -ResourceLine $resource
    }
    finally {
        Stop-Client -Process $client
    }
}

try {
    if ($Mode -contains 'empty') {
        for ($index = 1; $index -le $Samples; $index++) {
            $sample = '{0}-empty-{1:D2}' -f $runId, $index
            Invoke-ClientSample -Startup 'empty' -Sample $sample -EmptyWindow
        }
    }

    if ($Mode -contains 'warm') {
        $instance = "$instanceBase-w"
        $daemonSample = "$runId-warm-daemon"
        $daemon = Start-WithEnvironment -FilePath $daemonPath -ArgumentList @('--instance', $instance) -Environment @{
            COMPI_PERF_LOG = '1'
            COMPI_PERF_SAMPLE = $daemonSample
        }
        Wait-Daemon -Instance $instance
        try {
            $warmup = "$runId-warmup"
            $resultCount = $results.Count
            Invoke-ClientSample -Startup 'warm' -Sample $warmup -Instance $instance
            while ($results.Count -gt $resultCount) {
                $results.RemoveAt($results.Count - 1)
            }
            for ($index = 1; $index -le $Samples; $index++) {
                $sample = '{0}-warm-{1:D2}' -f $runId, $index
                Invoke-ClientSample -Startup 'warm' -Sample $sample -Instance $instance
            }
        }
        finally {
            Stop-Daemon -Instance $instance
        }
        foreach ($sessionCount in @(1, 2, 4)) {
            $instance = "$instanceBase-s$sessionCount"
            $sample = '{0}-sessions-{1}' -f $runId, $sessionCount
            $daemonSample = "$sample-daemon"
            $startup = "marginal-$sessionCount"
            $daemon = Start-WithEnvironment -FilePath $daemonPath `
                -ArgumentList @('--instance', $instance) -Environment @{
                    COMPI_PERF_LOG = '1'
                    COMPI_PERF_SAMPLE = $daemonSample
                }
            Wait-Daemon -Instance $instance
            try {
                Invoke-ClientSample -Startup $startup -Sample $sample `
                    -Instance $instance -SessionCount $sessionCount
                $daemonResource = Wait-ResourceSample -Process $daemon -ProcessKind 'daemon' `
                    -Sample $daemonSample -ExpectedSessions $sessionCount
                Add-ProcessMeasurement -Sample $daemonSample -Startup $startup `
                    -ProcessKind 'daemon' -Process $daemon -ResourceLine $daemonResource
            }
            finally {
                Stop-Daemon -Instance $instance
            }
        }
    }

    if ($Mode -contains 'cold') {
        for ($index = 1; $index -le $Samples; $index++) {
            $instance = '{0}-c{1:D2}' -f $instanceBase, $index
            $sample = '{0}-cold-{1:D2}' -f $runId, $index
            try {
                Invoke-ClientSample -Startup 'cold' -Sample $sample -Instance $instance
                $daemon = Get-DaemonProcess -Instance $instance
                $daemonResource = Wait-ResourceSample -Process $daemon -ProcessKind 'daemon' -Sample $sample
                Add-ProcessMeasurement -Sample $sample -Startup 'cold' -ProcessKind 'daemon' `
                    -Process $daemon -ResourceLine $daemonResource
            }
            finally {
                Stop-Daemon -Instance $instance
            }
        }
    }
}
finally {
    foreach ($process in $startedProcesses) {
        if (-not $process.HasExited) {
            Stop-Process -Id $process.Id -ErrorAction SilentlyContinue
        }
    }
}

function Get-WslRecord {
    $startInfo = [System.Diagnostics.ProcessStartInfo]::new()
    $startInfo.FileName = "$env:SystemRoot\System32\wsl.exe"
    $startInfo.Arguments = '--list --verbose'
    $startInfo.UseShellExecute = $false
    $startInfo.RedirectStandardOutput = $true
    $startInfo.RedirectStandardError = $true
    $startInfo.StandardOutputEncoding = [System.Text.Encoding]::Unicode
    $startInfo.StandardErrorEncoding = [System.Text.Encoding]::Unicode
    $process = [System.Diagnostics.Process]::Start($startInfo)
    $output = $process.StandardOutput.ReadToEnd()
    $errorOutput = $process.StandardError.ReadToEnd()
    $process.WaitForExit()
    if ($process.ExitCode -ne 0) {
        return $errorOutput.Trim()
    }
    return $output.Trim()
}

$qualified = $ConfirmPhysicalDisplay.IsPresent -and $Samples -ge 10 -and
    ($Mode -contains 'warm') -and ($Mode -contains 'cold') -and ($Mode -contains 'empty')
$environmentRecord = [ordered]@{
    run_id = $runId
    qualified_physical_display_run = $qualified
    physical_display_confirmed_by_operator = $ConfirmPhysicalDisplay.IsPresent
    samples_per_mode = $Samples
    modes = $Mode
    windows = [Environment]::OSVersion.VersionString
    cpu = @(Get-CimInstance Win32_Processor | Select-Object -ExpandProperty Name)
    video = @(Get-CimInstance Win32_VideoController | Select-Object Name, CurrentHorizontalResolution, CurrentVerticalResolution, CurrentRefreshRate)
    wsl = Get-WslRecord
    commit = (& git rev-parse HEAD 2>$null)
}

$csvPath = Join-Path $measurementDirectory "$runId.csv"
$jsonPath = Join-Path $measurementDirectory "$runId-environment.json"
$results | Export-Csv -Path $csvPath -NoTypeInformation
$environmentRecord | ConvertTo-Json -Depth 5 | Set-Content -Path $jsonPath -Encoding utf8

function Write-Distribution {
    param(
        [Parameter(Mandatory)] [string]$Startup,
        [Parameter(Mandatory)] [string]$Property
    )
    $values = @(
        $results |
            Where-Object {
                $_.process_kind -eq 'client' -and
                $_.startup -eq $Startup -and
                $null -ne $_.$Property
            } |
            ForEach-Object { [int64]$_.$Property } |
            Sort-Object
    )
    if ($values.Count -eq 0) {
        return
    }
    $p50 = $values[[math]::Ceiling($values.Count * 0.50) - 1]
    $p95 = $values[[math]::Ceiling($values.Count * 0.95) - 1]
    $worst = $values[-1]
    Write-Host "$Startup $Property p50=$p50 p95=$p95 worst=$worst"
}

Write-Host "Measurement CSV: $csvPath"
Write-Host "Environment JSON: $jsonPath"
Write-Host "Qualified physical-display run: $qualified"
foreach ($startup in $Mode) {
    Write-Distribution -Startup $startup -Property 'first_window_ms'
    Write-Distribution -Startup $startup -Property 'first_terminal_ms'
    Write-Distribution -Startup $startup -Property 'ready_for_input_ms'
    Write-Distribution -Startup $startup -Property 'input_to_render_ms'
}
