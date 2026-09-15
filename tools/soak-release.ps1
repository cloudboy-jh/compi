[CmdletBinding()]
param(
    [string]$BinaryDirectory,
    [ValidateRange(1, 1440)]
    [int]$Minutes = 30,
    [ValidateRange(5, 300)]
    [int]$SampleSeconds = 30,
    [ValidateRange(1, 8)]
    [int]$LoadSessions = 3,
    [string]$FontFamily = 'Cascadia Mono',
    [ValidateRange(6, 72)]
    [double]$FontSize = 14,
    [ValidateRange(0.8, 3.0)]
    [double]$LineHeight = 1.35,
    [string]$Theme = 'dark-glass',
    [switch]$ConfirmPhysicalDisplay
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
$contextPath = Join-Path $outputDirectory ("{0}-soak-environment.json" -f $instance)
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

function ConvertTo-NativeArgument {
    param([AllowEmptyString()] [string]$Argument)

    if ($Argument -notmatch '[\s"]') {
        return $Argument
    }
    if ($Argument.Contains('"')) {
        throw 'Soak process arguments cannot contain quotes'
    }
    return '"' + $Argument + '"'
}

function Get-Sha256 {
    param([Parameter(Mandatory)] [string]$Path)

    $stream = [IO.File]::OpenRead($Path)
    $sha = [Security.Cryptography.SHA256]::Create()
    try {
        return ([BitConverter]::ToString($sha.ComputeHash($stream))).Replace('-', '')
    }
    finally {
        $sha.Dispose()
        $stream.Dispose()
    }
}

function Start-SoakClient {
    $startInfo = [Diagnostics.ProcessStartInfo]::new()
    $startInfo.FileName = $clientPath
    $startInfo.UseShellExecute = $false
    $arguments = @(
        '--instance', $instance,
        '--font-family', $FontFamily,
        '--font-size', ([string]::Format([Globalization.CultureInfo]::InvariantCulture, '{0}', $FontSize)),
        '--line-height', ([string]::Format([Globalization.CultureInfo]::InvariantCulture, '{0}', $LineHeight)),
        '--theme', $Theme
    )
    $startInfo.Arguments = (($arguments | ForEach-Object {
        ConvertTo-NativeArgument -Argument $_
    }) -join ' ')
    $process = [Diagnostics.Process]::Start($startInfo)
    $script:processes.Add($process)
    return $process
}

function Get-ReceiptId {
    param(
        [Parameter(Mandatory)] [string]$Receipt,
        [Parameter(Mandatory)] [string]$Field
    )

    if ($Receipt -notmatch "(?:^|\s)$([regex]::Escape($Field))=([^\s]+)") {
        throw "Probe receipt did not contain ${Field}: $Receipt"
    }
    return $Matches[1].Split(',')[0]
}

function Wait-SessionRemoved {
    param([Parameter(Mandatory)] [string]$SessionId)

    $deadline = [DateTime]::UtcNow.AddSeconds(15)
    while ([DateTime]::UtcNow -lt $deadline) {
        $workspace = (& $probePath --instance $instance workspace | Out-String | ConvertFrom-Json)
        if ($LASTEXITCODE -eq 0 -and $workspace.sessions.id -notcontains $SessionId) {
            return
        }
        Start-Sleep -Milliseconds 100
    }
    throw "Lifecycle-cycle session $SessionId was not removed"
}

try {
    $daemon = Start-Process -FilePath $daemonPath -ArgumentList @('--instance', $instance) -PassThru -WindowStyle Hidden
    $processes.Add($daemon)
    $deadline = [DateTime]::UtcNow.AddSeconds(15)
    do {
        & $probePath --instance $instance workspace *> $null
        if ($LASTEXITCODE -eq 0) { break }
        Start-Sleep -Milliseconds 100
    } while ([DateTime]::UtcNow -lt $deadline)
    if ($LASTEXITCODE -ne 0) { throw 'Soak daemon did not become ready' }

    $durationSeconds = $Minutes * 60 + 30
    $loads = [System.Collections.Generic.List[System.Diagnostics.Process]]::new()
    for ($index = 0; $index -lt $LoadSessions; $index++) {
        $process = Start-Process -FilePath $probePath -ArgumentList @(
            '--instance', $instance, 'soak', $durationSeconds
        ) -PassThru -WindowStyle Hidden
        $processes.Add($process)
        $loads.Add($process)

        $expectedSurfaces = $index + 1
        $deadline = [DateTime]::UtcNow.AddSeconds(30)
        do {
            if ($process.HasExited) {
                throw "Sustained-output workload $($process.Id) exited with code $($process.ExitCode) during startup"
            }
            $workspace = (& $probePath --instance $instance workspace | Out-String | ConvertFrom-Json)
            if ($LASTEXITCODE -eq 0 -and @($workspace.surfaces).Count -ge $expectedSurfaces) {
                break
            }
            Start-Sleep -Milliseconds 100
        } while ([DateTime]::UtcNow -lt $deadline)
        if (@($workspace.surfaces).Count -lt $expectedSurfaces) {
            throw "Sustained-output workload $($process.Id) did not create a surface"
        }
    }
    Start-Sleep -Seconds 2
    $client = Start-SoakClient
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

        $sessionReceipt = (& $probePath --instance $instance session create "Soak cycle $sample").Trim()
        if ($LASTEXITCODE -ne 0) {
            throw 'Could not create the lifecycle-cycle session'
        }
        $cycleSession = Get-ReceiptId -Receipt $sessionReceipt -Field 'sessions'
        & $probePath --instance $instance tab create $cycleSession "Soak cycle $sample" *> $null
        if ($LASTEXITCODE -ne 0) { throw 'Could not create the lifecycle-cycle tab' }
        & $probePath --instance $instance session remove $cycleSession *> $null
        if ($LASTEXITCODE -ne 0) { throw 'Could not remove the lifecycle-cycle session' }
        Wait-SessionRemoved -SessionId $cycleSession

        $sample++
        Start-Sleep -Seconds $SampleSeconds
    }

    foreach ($load in $loads) {
        if (-not $load.WaitForExit(45000) -or $load.ExitCode -ne 0) {
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
    $qualified = $ConfirmPhysicalDisplay.IsPresent -and $Minutes -ge 30
    $context = [ordered]@{
        run_id = $instance
        qualified_physical_display_run = $qualified
        physical_display_confirmed_by_operator = $ConfirmPhysicalDisplay.IsPresent
        minutes = $Minutes
        sample_seconds = $SampleSeconds
        load_sessions = $LoadSessions
        binary_directory = (Resolve-Path $BinaryDirectory).Path
        build_profile = Split-Path -Leaf (Resolve-Path $BinaryDirectory).Path
        font_family = $FontFamily
        font_size = $FontSize
        line_height = $LineHeight
        theme = $Theme
        windows = [Environment]::OSVersion.VersionString
        cpu = @(Get-CimInstance Win32_Processor | Select-Object -ExpandProperty Name)
        video = @(Get-CimInstance Win32_VideoController | Select-Object Name, CurrentHorizontalResolution, CurrentVerticalResolution, CurrentRefreshRate)
        commit = (& git rev-parse HEAD 2>$null)
        rustc = (& rustc --version 2>$null)
        binaries = [ordered]@{
            client_sha256 = Get-Sha256 -Path $clientPath
            daemon_sha256 = Get-Sha256 -Path $daemonPath
            probe_sha256 = Get-Sha256 -Path $probePath
        }
    }
    $context | ConvertTo-Json -Depth 5 | Set-Content -Path $contextPath -Encoding utf8
    Write-Host "Environment JSON: $contextPath"
    Write-Host "Qualified physical-display run: $qualified"
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
