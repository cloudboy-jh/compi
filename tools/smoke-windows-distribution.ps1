[CmdletBinding()]
param([switch]$Install)

Set-StrictMode -Version Latest
$ErrorActionPreference = 'Stop'
$projectRoot = (Resolve-Path (Join-Path $PSScriptRoot '..')).Path
$distribution = Join-Path $projectRoot 'target\distribution'
$evidence = Join-Path $projectRoot 'target\distribution-smoke'
$installedDirectory = Join-Path $env:LOCALAPPDATA 'Programs\Compi'
$registration = 'HKCU:\Software\Microsoft\Windows\CurrentVersion\Uninstall\Compi'
if ($Install) {
    # MSI uninstall affects the account's default daemon and scheduled task.
    if ($env:GITHUB_ACTIONS -ne 'true' -or $env:RUNNER_ENVIRONMENT -ne 'github-hosted') {
        throw 'Installer smoke requires a disposable GitHub-hosted runner; use portable smoke locally'
    }
    if ((Test-Path $installedDirectory) -or (Test-Path $registration) -or
        (Get-ScheduledTask -TaskName 'Compi Daemon' -ErrorAction SilentlyContinue)) {
        throw 'Refusing to overwrite an existing Compi installation or scheduled task'
    }
}
New-Item -ItemType Directory -Force $evidence | Out-Null
$portable = @(Get-ChildItem $distribution -Filter 'Compi-*-Windows-x64.zip')
if ($portable.Count -ne 1) { throw 'Expected exactly one Windows portable ZIP' }
$portableDirectory = Join-Path $evidence ('portable-' + [guid]::NewGuid().ToString('N'))
Expand-Archive -LiteralPath $portable[0].FullName -DestinationPath $portableDirectory
if (-not (Test-Path (Join-Path $portableDirectory 'LICENSE'))) { throw 'Portable license is missing' }
& cargo build --locked -p compi-client --example compi-probe
if ($LASTEXITCODE -ne 0) { throw 'Failed to build verification probe' }
$probe = Join-Path $projectRoot 'target\debug\examples\compi-probe.exe'
$startupLog = Join-Path $env:LOCALAPPDATA 'Compi\client-startup.log'

function Wait-Condition {
    param([scriptblock]$Condition, [string]$Description, [int]$Seconds = 60)
    $deadline = [DateTime]::UtcNow.AddSeconds($Seconds)
    do {
        if (& $Condition) { return }
        Start-Sleep -Milliseconds 100
    } while ([DateTime]::UtcNow -lt $deadline)
    throw "Timed out waiting for $Description"
}

function Invoke-ClientSmoke {
    param([string]$Directory, [string]$Label)
    $instance = 'pkg-' + [guid]::NewGuid().ToString('N').Substring(0, 12)
    $client = $null
    $daemon = $null
    $previousEnvironment = @{}
    foreach ($key in @('COMPI_PERF_LOG', 'COMPI_PERF_SAMPLE')) {
        $previousEnvironment[$key] = [Environment]::GetEnvironmentVariable($key)
    }
    try {
        $identity = $null
        foreach ($launch in 1..2) {
            $sample = "$instance-$launch"
            $env:COMPI_PERF_LOG = '1'
            $env:COMPI_PERF_SAMPLE = $sample
            $client = Start-Process -FilePath (Join-Path $Directory 'compi.exe') -ArgumentList @('--instance', $instance) -PassThru
            Wait-Condition -Description "$Label launch $launch initial terminal snapshot" -Condition {
                if ($client.HasExited) { throw "Client exited with $($client.ExitCode)" }
                if (-not (Test-Path $startupLog)) { return $false }
                $lines = Get-Content $startupLog
                return [bool]($lines -match "sample=$sample .*metric=first_terminal_frame_ms ")
            }
            $workspaceJson = & $probe --instance $instance workspace
            if ($LASTEXITCODE -ne 0) { throw 'Packaged client did not establish a readable workspace' }
            $workspaceJson | Set-Content (Join-Path $evidence "$Label-$launch-workspace.json")
            $workspace = $workspaceJson | ConvertFrom-Json
            $surfaces = @($workspace.surfaces)
            if ($surfaces.Count -ne 1 -or $surfaces[0].status -ne 'running' -or -not $surfaces[0].attached) {
                throw 'Expected exactly one running packaged terminal'
            }
            $currentIdentity = "$($workspace.server_id)/$($workspace.server_generation)/$($surfaces[0].id)/$($surfaces[0].process_lifetime_id)"
            if ($launch -eq 1) {
                $identity = $currentIdentity
                $matches = @(Get-CimInstance Win32_Process -Filter "Name = 'compi-daemon.exe'" |
                    Where-Object { $_.CommandLine -match "--instance $instance(?:\s|$)" })
                if ($matches.Count -ne 1 -or $matches[0].ExecutablePath -ne (Join-Path $Directory 'compi-daemon.exe')) {
                    throw 'Client did not start its packaged sibling daemon'
                }
                $daemon = Get-Process -Id $matches[0].ProcessId
            } elseif ($identity -ne $currentIdentity) {
                throw 'Relaunch replaced the existing terminal or daemon identity'
            }
            if (-not $client.CloseMainWindow()) { throw 'Native client window could not be closed' }
            if (-not $client.WaitForExit(10000)) { throw 'Native client did not close' }
            $client.Dispose()
            $client = $null
        }
        Write-Host "${Label}: native launch, running attached PTY, sibling daemon, and same-process reconnect passed"
    }
    finally {
        if ($client -and -not $client.HasExited) { $client.Kill(); $client.WaitForExit(5000) | Out-Null }
        if (-not $daemon) {
            $owned = @(Get-CimInstance Win32_Process -Filter "Name = 'compi-daemon.exe'" |
                Where-Object {
                    $_.ExecutablePath -eq (Join-Path $Directory 'compi-daemon.exe') -and
                    $_.CommandLine -match "--instance $instance(?:\s|$)"
                })
            if ($owned.Count -eq 1) { $daemon = Get-Process -Id $owned[0].ProcessId }
        }
        & $probe --instance $instance shutdown
        if ($daemon -and -not $daemon.WaitForExit(10000)) { $daemon.Kill(); $daemon.WaitForExit(5000) | Out-Null }
        foreach ($key in $previousEnvironment.Keys) {
            [Environment]::SetEnvironmentVariable($key, $previousEnvironment[$key])
        }
    }
}

function Invoke-Msi {
    param([string]$Operation, [string]$Label)
    $msi = Join-Path $projectRoot 'target\installer\Compi.msi'
    $log = Join-Path $evidence "$Label-msi.log"
    $process = Start-Process msiexec.exe -ArgumentList "$Operation `"$msi`" /qn /norestart /L*v `"$log`"" -PassThru
    if (-not $process.WaitForExit(120000)) { throw "MSI $Label timed out; inspect $log" }
    if ($process.ExitCode -notin @(0, 1641, 3010)) { throw "MSI $Label failed ($($process.ExitCode)); inspect $log" }
}

try {
    Invoke-ClientSmoke $portableDirectory 'portable'
    if ($Install) {
        try {
            Invoke-Msi '/i' 'install'
            if (-not (Test-Path $registration)) { throw 'Installed product registration missing' }
            $task = Get-ScheduledTask -TaskName 'Compi Daemon'
            if ($task.Actions.Execute -ne (Join-Path $installedDirectory 'compi-daemon.exe')) {
                throw 'Scheduled task points outside installed product'
            }
            Invoke-ClientSmoke $installedDirectory 'installed'
            Remove-Item (Join-Path $installedDirectory 'compi.exe')
            Invoke-Msi '/fa' 'repair'
            if (-not (Test-Path (Join-Path $installedDirectory 'compi.exe'))) { throw 'Repair did not restore the client' }
            Invoke-ClientSmoke $installedDirectory 'repaired'
        }
        finally {
            if (Test-Path $registration) { Invoke-Msi '/x' 'uninstall' }
        }
        if ((Test-Path $registration) -or (Test-Path (Join-Path $installedDirectory 'compi.exe')) -or
            (Get-ScheduledTask -TaskName 'Compi Daemon' -ErrorAction SilentlyContinue)) {
            throw 'Uninstall left product files, registration, or scheduled task'
        }
        Write-Host 'MSI install, repair of missing client, and uninstall passed on disposable runner'
    }
}
finally {
    if (Test-Path $startupLog) { Copy-Item $startupLog (Join-Path $evidence 'client-startup.log') }
    Remove-Item -Recurse -Force $portableDirectory
}
