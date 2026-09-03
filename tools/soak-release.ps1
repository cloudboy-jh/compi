[CmdletBinding()]
param(
    [string]$BinaryDirectory,
    [ValidateRange(1, 1440)]
    [int]$Minutes = 30,
    [ValidateRange(5, 300)]
    [int]$SampleSeconds = 30,
    [ValidateRange(1, 8)]
    [int]$LoadSessions = 3
)

Set-StrictMode -Version Latest
if (-not $BinaryDirectory) {
    $BinaryDirectory = Join-Path $PSScriptRoot '..\target\release'
}
$ErrorActionPreference = 'Stop'
$clientPath = Join-Path $BinaryDirectory 'compi.exe'
$daemonPath = Join-Path $BinaryDirectory 'compi-daemon.exe'
$probePath = Join-Path $BinaryDirectory 'examples\compi-probe.exe'
foreach ($path in @($clientPath, $daemonPath, $probePath)) {
    if (-not (Test-Path $path -PathType Leaf)) {
        throw "Required soak binary was not found: $path"
    }
}

$instance = 'soak{0:MMddHHmmss}{1}' -f (Get-Date), $PID
$outputDirectory = Join-Path $env:LOCALAPPDATA 'Compi\measurements'
New-Item -ItemType Directory -Path $outputDirectory -Force | Out-Null
$outputPath = Join-Path $outputDirectory ("{0}-soak.csv" -f $instance)
$processes = [System.Collections.Generic.List[System.Diagnostics.Process]]::new()
$results = [System.Collections.Generic.List[object]]::new()
$daemon = $null
$client = $null

function Get-GpuMemory {
    param([Parameter(Mandatory)] [int]$ProcessId)

    try {
        $sample = Get-Counter @(
            '\GPU Process Memory(*)\Dedicated Usage',
            '\GPU Process Memory(*)\Shared Usage'
        ) -ErrorAction Stop
        $matching = $sample.CounterSamples | Where-Object {
            $_.InstanceName -match "pid_${ProcessId}_"
        }
        return @(
            ($matching | Where-Object Path -Like '*Dedicated Usage' | Measure-Object CookedValue -Sum).Sum,
            ($matching | Where-Object Path -Like '*Shared Usage' | Measure-Object CookedValue -Sum).Sum
        )
    }
    catch {
        return @($null, $null)
    }
}

function Add-ResourceSample {
    param(
        [Parameter(Mandatory)] [System.Diagnostics.Process]$Process,
        [Parameter(Mandatory)] [string]$Kind,
        [Parameter(Mandatory)] [int]$Ordinal
    )

    $Process.Refresh()
    if ($Process.HasExited) {
        throw "$Kind process $($Process.Id) exited with code $($Process.ExitCode) during the soak"
    }
    $gpu = Get-GpuMemory -ProcessId $Process.Id
    $script:results.Add([pscustomobject]@{
        timestamp = [DateTime]::UtcNow.ToString('o')
        sample = $Ordinal
        process = $Kind
        pid = $Process.Id
        private_bytes = $Process.PrivateMemorySize64
        working_set_bytes = $Process.WorkingSet64
        handles = $Process.HandleCount
        gpu_dedicated_bytes = $gpu[0]
        gpu_shared_bytes = $gpu[1]
    })
}

try {
    $daemon = Start-Process -FilePath $daemonPath -ArgumentList @('--instance', $instance) -PassThru -WindowStyle Hidden
    $processes.Add($daemon)
    $deadline = [DateTime]::UtcNow.AddSeconds(15)
    do {
        & $probePath --instance $instance list *> $null
        if ($LASTEXITCODE -eq 0) { break }
        Start-Sleep -Milliseconds 100
    } while ([DateTime]::UtcNow -lt $deadline)
    if ($LASTEXITCODE -ne 0) { throw 'Soak daemon did not become ready' }

    $durationSeconds = $Minutes * 60 + 45
    $loads = for ($index = 0; $index -lt $LoadSessions; $index++) {
        $process = Start-Process -FilePath $probePath -ArgumentList @(
            '--instance', $instance, 'soak', $durationSeconds
        ) -PassThru -WindowStyle Hidden
        $processes.Add($process)
        $process
    }
    Start-Sleep -Seconds 2
    $client = Start-Process -FilePath $clientPath -ArgumentList @('--instance', $instance) -PassThru -WindowStyle Minimized
    $processes.Add($client)
    Start-Sleep -Seconds 6

    $started = [DateTime]::UtcNow
    $ends = $started.AddMinutes($Minutes)
    $sample = 0
    while ([DateTime]::UtcNow -lt $ends) {
        Add-ResourceSample -Process $daemon -Kind 'daemon' -Ordinal $sample
        Add-ResourceSample -Process $client -Kind 'client' -Ordinal $sample
        foreach ($load in $loads) {
            Add-ResourceSample -Process $load -Kind 'load' -Ordinal $sample
        }

        $cycleSession = (& $probePath --instance $instance create).Trim()
        if ($LASTEXITCODE -ne 0 -or -not $cycleSession) {
            throw 'Could not create the lifecycle-cycle session'
        }
        & $probePath --instance $instance kill $cycleSession *> $null
        if ($LASTEXITCODE -ne 0) { throw 'Could not kill the lifecycle-cycle session' }

        $sample++
        Start-Sleep -Seconds $SampleSeconds
    }

    foreach ($load in $loads) {
        if (-not $load.WaitForExit(30000) -or $load.ExitCode -ne 0) {
            throw "Sustained-output workload $($load.Id) did not exit cleanly"
        }
    }
    Add-ResourceSample -Process $daemon -Kind 'daemon' -Ordinal $sample
    Add-ResourceSample -Process $client -Kind 'client' -Ordinal $sample

    foreach ($kind in @('daemon', 'client')) {
        $samples = @($results | Where-Object process -EQ $kind | Sort-Object sample)
        if ($samples.Count -lt 2) { throw "Insufficient $kind resource samples" }
        $growth = $samples[-1].handles - $samples[0].handles
        if ($growth -gt 8) {
            throw "$kind handle count grew by $growth during the soak"
        }
    }
    $results | Export-Csv -Path $outputPath -NoTypeInformation -Encoding utf8
    Write-Host "Soak passed: $outputPath"
}
finally {
    if ($results.Count -gt 0 -and -not (Test-Path $outputPath)) {
        $results | Export-Csv -Path $outputPath -NoTypeInformation -Encoding utf8
    }
    if ($client -and -not $client.HasExited) {
        Stop-Process -Id $client.Id -Force -ErrorAction SilentlyContinue
    }
    foreach ($process in $processes) {
        if (
            (-not $daemon -or $process.Id -ne $daemon.Id) -and
            -not $process.HasExited
        ) {
            Stop-Process -Id $process.Id -Force -ErrorAction SilentlyContinue
        }
    }
    if ($daemon -and -not $daemon.HasExited) {
        & $probePath --instance $instance shutdown *> $null
        if (-not $daemon.WaitForExit(5000)) {
            Stop-Process -Id $daemon.Id -Force -ErrorAction SilentlyContinue
        }
    }
}
