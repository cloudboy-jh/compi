[CmdletBinding()]
param(
    [switch]$Install,
    [string]$DistributionDirectory,
    [string]$PreviousMsi,
    [string]$UpdateArtifact,
    [string]$UpdateMetadata,
    [string]$UpdateDriver,
    [string]$ProbePath
)

Set-StrictMode -Version Latest
$ErrorActionPreference = 'Stop'
$projectRoot = (Resolve-Path (Join-Path $PSScriptRoot '..')).Path
$distribution = if ($DistributionDirectory) {
    (Resolve-Path $DistributionDirectory).Path
} else {
    Join-Path $projectRoot 'target\distribution'
}
$evidence = Join-Path $projectRoot 'target\distribution-smoke'
$installedDirectory = Join-Path $env:LOCALAPPDATA 'Programs\Compi'
$registration = 'HKCU:\Software\Microsoft\Windows\CurrentVersion\Uninstall\Compi'
$currentUserSid = [System.Security.Principal.WindowsIdentity]::GetCurrent().User.Value
$taskName = "Compi Daemon-$currentUserSid"
if ($Install) {
    # MSI uninstall affects the account's default daemon and scheduled task.
    if ($env:GITHUB_ACTIONS -ne 'true' -or $env:RUNNER_ENVIRONMENT -ne 'github-hosted') {
        throw 'Installer smoke requires a disposable GitHub-hosted runner; use portable smoke locally'
    }
    if ((Test-Path $installedDirectory) -or (Test-Path $registration) -or
        (Get-ScheduledTask -TaskName $taskName -ErrorAction SilentlyContinue)) {
        throw 'Refusing to overwrite an existing Compi installation or scheduled task'
    }
    if ($PreviousMsi -and (Get-ScheduledTask -TaskName 'Compi Daemon' -ErrorAction SilentlyContinue)) {
        throw 'Legacy baseline MSI uses a fixed task name; refusing to collide with any existing legacy task'
    }
}
New-Item -ItemType Directory -Force $evidence | Out-Null
$portable = @(Get-ChildItem $distribution -Filter 'Compi-*-Windows-x64.zip')
if ($portable.Count -ne 1) { throw 'Expected exactly one Windows versioned portable ZIP' }
$portableDirectory = Join-Path $evidence ('portable-' + [guid]::NewGuid().ToString('N'))
Expand-Archive -LiteralPath $portable[0].FullName -DestinationPath $portableDirectory
$ownedMarker = Join-Path $evidence ".$([System.IO.Path]::GetFileName($portableDirectory)).compi-smoke-owned.json"
[System.IO.File]::WriteAllText($ownedMarker,
    (@{root = [System.IO.Path]::GetFullPath($portableDirectory); nonce = [guid]::NewGuid().ToString('N');
       artifact_sha256 = (Get-FileHash -Algorithm SHA256 $portable[0].FullName).Hash.ToLowerInvariant()} | ConvertTo-Json),
    [System.Text.UTF8Encoding]::new($false))
if (-not (Test-Path (Join-Path $portableDirectory 'LICENSE'))) { throw 'Portable license is missing' }
$selection = Get-Content -Raw (Join-Path $portableDirectory 'selection.json') | ConvertFrom-Json
if ($selection.schema -ne 1) { throw 'Unsupported portable selection schema' }
$version = $selection.version
$updatePayload = Join-Path $distribution "Compi-$version-Windows-x64-update.zip"
if (-not (Test-Path -LiteralPath $updatePayload)) { throw 'Full update payload is missing' }
$artifactPaths = @($portable[0].FullName, $updatePayload) + @(Get-ChildItem $distribution -Filter "Compi-$version-Setup.exe" | ForEach-Object FullName)
if ($PreviousMsi) { $artifactPaths += (Resolve-Path -LiteralPath $PreviousMsi).Path }
$artifactHashes = @($artifactPaths | ForEach-Object {
    @{path = $_; sha256 = (Get-FileHash -LiteralPath $_ -Algorithm SHA256).Hash.ToLowerInvariant()}
})
[System.IO.File]::WriteAllText((Join-Path $evidence 'artifact-hashes.json'),
    ($artifactHashes | ConvertTo-Json), [System.Text.UTF8Encoding]::new($false))
foreach ($path in @('compi.exe', 'compi-update-worker.exe', "versions\$version\compi.exe", "versions\$version\compi-daemon.exe", "versions\$version\conpty.dll", "versions\$version\OpenConsole.exe", "versions\$version\ConPTY-LICENSE.txt", "versions\$version\LICENSE")) {
    if (-not (Test-Path -LiteralPath (Join-Path $portableDirectory $path))) { throw "Portable package is missing $path" }
}
$isolatedData = Join-Path $evidence ('profile-' + [guid]::NewGuid().ToString('N'))
New-Item -ItemType Directory -Path $isolatedData | Out-Null
$originalData = $env:COMPI_DATA_DIR
$originalRuntime = $env:COMPI_RUNTIME_DIR
$originalLocalAppData = $env:LOCALAPPDATA
if (-not $Install) { $env:LOCALAPPDATA = $isolatedData }
$env:COMPI_DATA_DIR = Join-Path $env:LOCALAPPDATA 'Compi'
$env:COMPI_RUNTIME_DIR = Join-Path $isolatedData 'runtime'
New-Item -ItemType Directory -Force -Path $env:COMPI_DATA_DIR, $env:COMPI_RUNTIME_DIR | Out-Null
$managedSentinelDirectory = Join-Path $env:COMPI_DATA_DIR 'client-state-v1'
New-Item -ItemType Directory -Force -Path $managedSentinelDirectory | Out-Null
$sentinel = Join-Path $managedSentinelDirectory 'preservation-sentinel.txt'
$sentinelValue = [guid]::NewGuid().ToString('N')
[System.IO.File]::WriteAllText($sentinel, $sentinelValue)
$externalSentinel = Join-Path $isolatedData 'external-project-sentinel.txt'
[System.IO.File]::WriteAllText($externalSentinel, $sentinelValue)
$unknownProject = Join-Path $env:COMPI_DATA_DIR 'unmanaged-project'
New-Item -ItemType Directory -Force -Path $unknownProject | Out-Null
$unknownProjectSentinel = Join-Path $unknownProject 'preservation-sentinel.txt'
[System.IO.File]::WriteAllText($unknownProjectSentinel, $sentinelValue)
if (-not $ProbePath) {
    & cargo build --locked -p compi-client --example compi-probe --example compi-update-smoke
    if ($LASTEXITCODE -ne 0) { throw 'Failed to build verification diagnostics' }
    $ProbePath = Join-Path $projectRoot 'target\debug\examples\compi-probe.exe'
}
$probe = (Resolve-Path -LiteralPath $ProbePath).Path
if (-not $UpdateDriver) { $UpdateDriver = Join-Path $projectRoot 'target\debug\examples\compi-update-smoke.exe' }
$startupLog = Join-Path $env:COMPI_DATA_DIR 'client-startup.log'

function Wait-Condition {
    param([scriptblock]$Condition, [string]$Description, [int]$Seconds = 60)
    $deadline = [DateTime]::UtcNow.AddSeconds($Seconds)
    do {
        if (& $Condition) { return }
        Start-Sleep -Milliseconds 100
    } while ([DateTime]::UtcNow -lt $deadline)
    throw "Timed out waiting for $Description"
}

function Resolve-PayloadDirectory {
    param([string]$Directory)
    $path = Join-Path $Directory 'selection.json'
    if (-not (Test-Path -LiteralPath $path)) { return $Directory }
    $selected = Get-Content -Raw $path | ConvertFrom-Json
    if ($selected.schema -ne 1 -or $selected.version -notmatch '^\d+\.\d+\.\d+$') { throw 'Invalid selected payload' }
    return Join-Path $Directory "versions\$($selected.version)"
}

function Assert-PreservedData {
    if (-not (Test-Path -LiteralPath $sentinel) -or [System.IO.File]::ReadAllText($sentinel) -ne $sentinelValue) {
        throw 'Installation operation changed disposable managed user data'
    }
    if (-not (Test-Path -LiteralPath $externalSentinel) -or [System.IO.File]::ReadAllText($externalSentinel) -ne $sentinelValue) {
        throw 'Installation operation changed an external project sentinel'
    }
    if (-not (Test-Path -LiteralPath $unknownProjectSentinel) -or [System.IO.File]::ReadAllText($unknownProjectSentinel) -ne $sentinelValue) {
        throw 'Installation operation changed an unknown project inside the data root'
    }
}

function Invoke-Wrapper {
    param([string]$Executable, [string]$Operation, [string]$Label, [switch]$RemoveData, [int]$CancelAfterMs = -1)
    $arguments = @("--$Operation")
    if ([System.IO.Path]::GetFileName($Executable) -eq 'Compi-Setup.exe') {
        $uninstall = (Get-ItemProperty -LiteralPath $registration).UninstallString
        if ($uninstall -notmatch '\{[0-9A-Fa-f-]{36}\}') { throw 'Installed product code is missing' }
        $arguments += $Matches[0]
    }
    $arguments += '--silent'
    if ($RemoveData) { $arguments += '--remove-data' }
    if ($CancelAfterMs -ge 0) { $arguments += @('--cancel-after-ms', "$CancelAfterMs") }
    $errorLog = Join-Path $evidence "$Label-wrapper.err.log"
    $process = Start-Process -FilePath $Executable -ArgumentList $arguments -PassThru `
        -RedirectStandardError $errorLog -RedirectStandardOutput (Join-Path $evidence "$Label-wrapper.out.log")
    if (-not $process.WaitForExit(120000)) { throw "Wrapper $Label timed out" }
    $process.WaitForExit()
    $detail = if (Test-Path -LiteralPath $errorLog) { (Get-Content -Raw -LiteralPath $errorLog) -as [string] } else { '' }
    if ($CancelAfterMs -ge 0) {
        if ($process.ExitCode -ne 1602) { throw "Cancellation was not observed ($($process.ExitCode)): $detail" }
    } elseif ($process.ExitCode -notin @(0, 1641, 3010)) { throw "Wrapper $Label failed ($($process.ExitCode)): $detail" }
    if (-not $RemoveData) { Assert-PreservedData }
    $installerScenarios.Add($Label)
}
function Invoke-ClientSmoke {
    param([string]$Directory, [string]$Label)
    $instance = 'pkg-' + [guid]::NewGuid().ToString('N').Substring(0, 12)
    $payload = Resolve-PayloadDirectory $Directory
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
            $launcher = Start-Process -FilePath (Join-Path $Directory 'compi.exe') -ArgumentList @('--instance', $instance) -PassThru
            Wait-Condition -Description "$Label launch $launch initial terminal snapshot" -Condition {
                $candidates = @(Get-CimInstance Win32_Process -Filter "Name = 'compi.exe'" |
                    Where-Object { $_.ExecutablePath -eq (Join-Path $payload 'compi.exe') -and $_.CommandLine -match "--instance $instance(?:\s|$)" })
                if ($candidates.Count -eq 1) { $script:observedClient = Get-Process -Id $candidates[0].ProcessId }
                if (-not (Test-Path $startupLog)) { return $false }
                $lines = Get-Content $startupLog
                return $candidates.Count -eq 1 -and [bool]($lines -match "sample=$sample .*metric=first_terminal_frame_ms ")
            }
            $client = $script:observedClient
            $launcher.Dispose()
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
                if ($matches.Count -ne 1 -or $matches[0].ExecutablePath -ne (Join-Path $payload 'compi-daemon.exe')) {
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
                    $_.ExecutablePath -eq (Join-Path $payload 'compi-daemon.exe') -and
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
    param([string]$Operation, [string]$Label, [string]$MsiPath = (Join-Path $projectRoot 'target\installer\Compi.msi'))
    $msi = (Resolve-Path -LiteralPath $MsiPath).Path
    $log = Join-Path $evidence "$Label-msi.log"
    $process = Start-Process msiexec.exe -ArgumentList "$Operation `"$msi`" /qn /norestart /L*v `"$log`"" -PassThru
    if (-not $process.WaitForExit(120000)) { throw "MSI $Label timed out; inspect $log" }
    if ($process.ExitCode -notin @(0, 1641, 3010)) { throw "MSI $Label failed ($($process.ExitCode)); inspect $log" }
}

$smokeCompleted = $false
$installerScenarios = [System.Collections.Generic.List[string]]::new()
$updateCycle = $null
try {
    Invoke-ClientSmoke $portableDirectory 'portable'
    if ($Install) {
        try {
            $setup = @(Get-ChildItem $distribution -Filter "Compi-$version-Setup.exe")
            if ($setup.Count -ne 1) { throw 'Expected one setup wrapper' }
            if ($PreviousMsi) {
                # Legacy A had no unattended wrapper/update CLI; install its genuine
                # MSI, then exercise the new wrapper for the deliberate migration.
                Invoke-Msi '/i' 'legacy-install' $PreviousMsi
                Invoke-ClientSmoke $installedDirectory 'legacy'
                # This first transition deliberately has no active legacy shells.
                # The old daemon cannot report stable live-work inventory. On this
                # disposable account explicitly stop its owned default generation.
                $legacyTask = Get-ScheduledTask -TaskName 'Compi Daemon'
                $legacyUser = $legacyTask.Principal.UserId
                $legacyOwnerSid = if ($legacyUser -match '^S-1-') {
                    ([System.Security.Principal.SecurityIdentifier]::new($legacyUser)).Value
                } else {
                    ([System.Security.Principal.NTAccount]::new($legacyUser)).Translate([System.Security.Principal.SecurityIdentifier]).Value
                }
                if ($legacyOwnerSid -ne $currentUserSid -or $legacyTask.Actions.Execute -ne (Join-Path $installedDirectory 'compi-daemon.exe')) {
                    throw 'Refusing to stop a legacy task not owned by this disposable baseline installation'
                }
                # The genuine baseline MSI registers this task without starting it.
                # Never let the current probe activate the new SID-named task while
                # waiting for the legacy default endpoint to become available.
                Start-ScheduledTask -InputObject $legacyTask
                Wait-Condition -Description "test-owned legacy default daemon endpoint (Compi Daemon; inspect Get-ScheduledTaskInfo LastTaskResult and $installedDirectory\compi-daemon.exe)" -Condition {
                    $legacyProcesses = @(Get-CimInstance Win32_Process -Filter "Name = 'compi-daemon.exe'" |
                        Where-Object { $_.ExecutablePath -eq (Join-Path $installedDirectory 'compi-daemon.exe') })
                    if ($legacyProcesses.Count -eq 0) { return $false }
                    $endpoint = [System.IO.Pipes.NamedPipeClientStream]::new(
                        '.', "compi-daemon-$currentUserSid", [System.IO.Pipes.PipeDirection]::InOut)
                    try {
                        $endpoint.Connect(100)
                        return $endpoint.IsConnected
                    } catch [System.TimeoutException] {
                        return $false
                    } finally {
                        $endpoint.Dispose()
                    }
                }
                $legacyWorkspaceJson = & $probe workspace
                if ($LASTEXITCODE -ne 0) { throw 'Cannot account for baseline default workspace; stop it deliberately before migration' }
                $legacyWorkspace = $legacyWorkspaceJson | ConvertFrom-Json
                if (@($legacyWorkspace.surfaces | Where-Object { $_.status -eq 'running' -or $_.attached }).Count -ne 0) {
                    throw 'Refusing legacy migration while default live work remains'
                }
                Disable-ScheduledTask -TaskName 'Compi Daemon' | Out-Null
                Stop-ScheduledTask -TaskName 'Compi Daemon' -ErrorAction SilentlyContinue
                & $probe shutdown
                if ($LASTEXITCODE -ne 0) { throw 'Deliberate legacy default-daemon stop failed' }
                Wait-Condition -Description 'test-owned legacy task and daemon completely inactive' -Condition {
                    $legacyState = (Get-ScheduledTask -TaskName 'Compi Daemon').State
                    $oldProcesses = @(Get-CimInstance Win32_Process -Filter "Name = 'compi-daemon.exe'" |
                        Where-Object { $_.ExecutablePath -eq (Join-Path $installedDirectory 'compi-daemon.exe') })
                    return $legacyState -notin @('Running', 'Queued') -and $oldProcesses.Count -eq 0
                }
            }
            Invoke-Wrapper $setup[0].FullName 'install' 'install'
            if (-not (Test-Path $registration)) { throw 'Installed product registration missing' }
            $task = Get-ScheduledTask -TaskName $taskName
            if ($task.Principal.UserId -ne $currentUserSid) { throw 'Scheduled task belongs to another user' }
            $installedPayload = Resolve-PayloadDirectory $installedDirectory
            if ($task.Actions.Execute -notin @((Join-Path $installedPayload 'compi-daemon.exe'), (Join-Path $installedDirectory 'compi.exe'))) {
                throw 'Scheduled task points outside the selected installed generation'
            }
            if ($task.Actions.Execute -eq (Join-Path $installedDirectory 'compi.exe') -and
                $task.Actions.Arguments -notmatch "--daemon-generation $version(?:\s|$)") {
                throw 'Scheduled supervisor must capture the exact generation, not follow mutable selection'
            }
            Invoke-ClientSmoke $installedDirectory 'installed'
            if ($PreviousMsi) {
                if (Get-ScheduledTask -TaskName 'Compi Daemon' -ErrorAction SilentlyContinue) {
                    throw 'Migration left the fixed legacy scheduled task registered'
                }
                $installerScenarios.Add('legacy-migration')
            }
            $maintenance = Join-Path $installedDirectory 'Compi-Setup.exe'
            $priorSelection = Get-Content -Raw (Join-Path $installedDirectory 'selection.json')
            Invoke-Wrapper $maintenance 'repair' 'cancel-repair' -CancelAfterMs 1
            if (-not (Test-Path $registration) -or (Get-Content -Raw (Join-Path $installedDirectory 'selection.json')) -ne $priorSelection) {
                throw 'Cancelled MSI repair failed to preserve installed selection/registration'
            }
            Remove-Item (Join-Path $installedPayload 'compi.exe')
            Invoke-Wrapper $maintenance 'repair' 'repair'
            if (-not (Test-Path (Join-Path $installedPayload 'compi.exe'))) { throw 'Repair did not restore the selected client' }
            Invoke-ClientSmoke $installedDirectory 'repaired'
        }
        catch {
            # Report the first failure; cleanup errors below must not mask it.
            Write-Host "Installer scenario failed after: $($installerScenarios -join ', ')"
            Write-Host "$_`n$($_.ScriptStackTrace)"
            throw
        }
        finally {
            $installerLog = Join-Path $env:LOCALAPPDATA 'Compi\installer.log'
            if (Test-Path -LiteralPath $installerLog) { Copy-Item -LiteralPath $installerLog (Join-Path $evidence 'installer.log') }
            if (Test-Path $registration) {
                try { Invoke-Wrapper (Join-Path $installedDirectory 'Compi-Setup.exe') 'remove' 'uninstall' }
                catch {
                    if (Test-Path -LiteralPath $installerLog) { Copy-Item -LiteralPath $installerLog (Join-Path $evidence 'installer.log') -Force }
                    throw
                }
            }
        }
        if ((Test-Path $registration) -or (Test-Path (Join-Path $installedDirectory 'compi.exe')) -or
            (Get-ScheduledTask -TaskName $taskName -ErrorAction SilentlyContinue) -or
            ($PreviousMsi -and (Get-ScheduledTask -TaskName 'Compi Daemon' -ErrorAction SilentlyContinue))) {
            throw 'Uninstall left product files, registration, or scheduled task'
        }
        Assert-PreservedData
        Invoke-Wrapper $setup[0].FullName 'install' 'reinstall-preserved-data'
        Invoke-ClientSmoke $installedDirectory 'reinstalled'
        Invoke-Wrapper (Join-Path $installedDirectory 'Compi-Setup.exe') 'remove' 'remove-reinstalled' -RemoveData
        if (Test-Path -LiteralPath $sentinel) { throw 'Explicit managed-data cleanup left the sentinel' }
        if ([System.IO.File]::ReadAllText($externalSentinel) -ne $sentinelValue) { throw 'Explicit cleanup changed external project data' }
        if ([System.IO.File]::ReadAllText($unknownProjectSentinel) -ne $sentinelValue) { throw 'Explicit cleanup changed an unknown project inside the data root' }
        Write-Host 'Setup/maintenance install, repair, default data retention, reinstall and removal passed on disposable runner'
    }
    if ($UpdateArtifact -or $UpdateMetadata) {
        if (-not $UpdateArtifact -or -not $UpdateMetadata) { throw 'Provide both UpdateArtifact and UpdateMetadata' }
        $priorCycles = @(Get-ChildItem -LiteralPath $evidence -Directory -Filter 'cycle-*' | ForEach-Object FullName)
        & python (Join-Path $PSScriptRoot 'smoke-update-cycle.py') --root $portableDirectory --artifact $UpdateArtifact --metadata $UpdateMetadata --probe $probe --driver $UpdateDriver --evidence $evidence
        if ($LASTEXITCODE -ne 0) { throw 'Version-to-version update smoke failed' }
        $completedCycles = @(Get-ChildItem -LiteralPath $evidence -Directory -Filter 'cycle-*' |
            Where-Object { $_.FullName -notin $priorCycles })
        if ($completedCycles.Count -ne 1) { throw 'Successful update smoke must produce exactly one new cycle evidence directory' }
        $cycleEvidence = Join-Path $completedCycles[0].FullName 'evidence.json'
        if (-not (Test-Path -LiteralPath $cycleEvidence)) { throw "Successful update smoke did not write its evidence: $cycleEvidence" }
        $updateCycle = Get-Content -Raw -LiteralPath $cycleEvidence | ConvertFrom-Json
    }
    $qualification = @{
        schema = 1; platform = 'windows-x86_64'; version = $version; daemon_protocol = 18
        qualified_daemons = @($version); artifact_sha256 = (Get-FileHash -Algorithm SHA256 $updatePayload).Hash.ToLowerInvariant()
        installer_scenarios = @($installerScenarios.ToArray())
    }
    if ($null -ne $updateCycle) { $qualification.update_cycle = $updateCycle }
    [System.IO.File]::WriteAllText((Join-Path $distribution 'qualification-windows-x86_64.json'),
        ($qualification | ConvertTo-Json -Depth 20), [System.Text.UTF8Encoding]::new($false))
    $smokeCompleted = $true
}
finally {
    try {
        if (Test-Path $startupLog) { Copy-Item $startupLog (Join-Path $evidence 'client-startup.log') }
        if ($smokeCompleted) {
            Remove-Item -Recurse -Force $portableDirectory
            Remove-Item -LiteralPath $ownedMarker -Force
        } else {
            Write-Warning "Failure retained disposable root and its runnable prior payload: $portableDirectory"
        }
    } finally {
        $env:COMPI_DATA_DIR = $originalData
        $env:COMPI_RUNTIME_DIR = $originalRuntime
        $env:LOCALAPPDATA = $originalLocalAppData
    }
}
