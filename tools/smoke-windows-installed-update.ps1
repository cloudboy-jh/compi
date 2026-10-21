[CmdletBinding()]
param([string]$DistributionDirectory = 'target/distribution')

Set-StrictMode -Version Latest
$ErrorActionPreference = 'Stop'
# Never install, stop a daemon, or touch a task on a developer/self-hosted account.
if (-not $IsWindows -or $env:GITHUB_ACTIONS -ne 'true' -or $env:RUNNER_ENVIRONMENT -ne 'github-hosted') {
    throw 'Installed update qualification requires a fresh native GitHub-hosted Windows runner'
}
if ($env:COMPI_UPDATE_SIGNING_KEY) { throw 'Production metadata signing secrets must not enter this fixture' }
$projectRoot = (Resolve-Path (Join-Path $PSScriptRoot '..')).Path
$distribution = (Resolve-Path -LiteralPath $DistributionDirectory).Path
$root = Join-Path $env:LOCALAPPDATA 'Programs\Compi'
$data = Join-Path $env:LOCALAPPDATA 'Compi'
$registration = 'HKCU:\Software\Microsoft\Windows\CurrentVersion\Uninstall\Compi'
$sid = [Security.Principal.WindowsIdentity]::GetCurrent().User.Value
$taskName = "Compi Daemon-$sid"
$existingTasks = @(Get-ScheduledTask | Where-Object { $_.TaskName -like 'Compi Daemon*' })
$existingProcesses = @(Get-CimInstance Win32_Process | Where-Object { $_.Name -in @('compi.exe', 'compi-daemon.exe', 'compi-update-worker.exe', 'compi-launcher.exe') })
$existingRegistrations = @(foreach ($hive in @('HKCU:', 'HKLM:')) {
    foreach ($branch in @('Software\Microsoft\Windows\CurrentVersion\Uninstall', 'Software\WOW6432Node\Microsoft\Windows\CurrentVersion\Uninstall')) {
        $uninstallRoot = Join-Path $hive $branch
        if (Test-Path -LiteralPath $uninstallRoot) {
            Get-ChildItem -LiteralPath $uninstallRoot | ForEach-Object {
                $entry = Get-ItemProperty -LiteralPath $_.PSPath
                $displayName = $entry.PSObject.Properties['DisplayName']
                if ($_.PSChildName -eq 'Compi' -or ($displayName -and $displayName.Value -match '^Compi(?:\s|$)')) { $_.PSPath }
            }
        }
    }
})
if ((Test-Path -LiteralPath $root) -or (Test-Path -LiteralPath $data) -or
    (Test-Path -LiteralPath $registration) -or $existingRegistrations.Count -or $existingTasks.Count -or $existingProcesses.Count) {
    throw 'Refusing an account with existing Compi files, data, registration, daemon, GUI, or task'
}
foreach ($name in @('COMPI_DATA_DIR', 'COMPI_RUNTIME_DIR', 'COMPI_CONFIG_FILE', 'COMPI_INSTANCE', 'COMPI_SUPERVISOR_PID')) {
    if ([Environment]::GetEnvironmentVariable($name)) { throw "Refusing inherited ${name}: fixture must use the actual disposable runner account" }
}
$manifest = [IO.File]::ReadAllText((Join-Path $projectRoot 'Cargo.toml'))
$versionMatch = [regex]::Match($manifest, '(?ms)^\[workspace\.package\]\s*\r?\nversion = "(\d+\.\d+\.\d+)"')
if (-not $versionMatch.Success) { throw 'Cannot read exact workspace baseline version' }
$version = $versionMatch.Groups[1].Value
$parsed = [version]$version
$futureVersion = "$($parsed.Major).$($parsed.Minor).$($parsed.Build + 1)"
$setup = Join-Path $distribution "Compi-$version-Setup.exe"
$portable = Join-Path $distribution "Compi-$version-Windows-x64.zip"
foreach ($path in @($setup, $portable)) { if (-not (Test-Path -LiteralPath $path -PathType Leaf)) { throw "Missing official packaged artifact: $path" } }
$run = Join-Path $projectRoot ('target\distribution-smoke\installed-' + [guid]::NewGuid().ToString('N'))
$scaffold = Join-Path $env:RUNNER_TEMP ('compi-installed-fixture-' + [guid]::NewGuid().ToString('N'))
$source = Join-Path $scaffold 'source'
$build = Join-Path $projectRoot 'target'
$privateKey = Join-Path $scaffold 'disposable-ed25519.key'
$marker = Join-Path (Split-Path $root -Parent) '.Compi.compi-smoke-owned.json'
$nonce = [guid]::NewGuid().ToString('N')
$originalKey = $env:COMPI_UPDATE_PUBLIC_KEY
$originalTarget = $env:CARGO_TARGET_DIR
$originalFxc = $env:GPUI_FXC_PATH
$owned = $false
$probe = $null
$failure = $null
$cleanupErrors = [Collections.Generic.List[string]]::new()
$utf8 = [Text.UTF8Encoding]::new($false)
New-Item -ItemType Directory -Path $run, $source -Force | Out-Null

function Write-Json([string]$Path, $Value) {
    [IO.File]::WriteAllText($Path, ($Value | ConvertTo-Json -Depth 30), $utf8)
}
function Hash([string]$Path) { (Get-FileHash -LiteralPath $Path -Algorithm SHA256).Hash.ToLowerInvariant() }
function Wait-Condition([scriptblock]$Condition, [string]$Description, [int]$Seconds = 60) {
    $deadline = [DateTime]::UtcNow.AddSeconds($Seconds)
    do {
        if (& $Condition) { return }
        Start-Sleep -Milliseconds 150
    } while ([DateTime]::UtcNow -lt $deadline)
    throw "Timed out waiting for $Description"
}
function Invoke-Cargo([string[]]$Arguments, [string]$Label) {
    & cargo @Arguments 2>&1 | Tee-Object -FilePath (Join-Path $run "$Label.log") | Out-Host
    if ($LASTEXITCODE -ne 0) { throw "Cargo fixture build failed: $Label ($LASTEXITCODE)" }
}
function Invoke-Setup([string]$Executable, [string]$Operation, [string]$Label) {
    $arguments = @("--$Operation")
    if ([IO.Path]::GetFileName($Executable) -eq 'Compi-Setup.exe') {
        $uninstall = (Get-ItemProperty -LiteralPath $registration).UninstallString
        if ($uninstall -notmatch '\{[0-9A-Fa-f-]{36}\}') { throw 'Installed product code is missing' }
        $arguments += $Matches[0]
    }
    $arguments += '--silent'
    $process = Start-Process -FilePath $Executable -ArgumentList $arguments -PassThru `
        -RedirectStandardOutput (Join-Path $run "$Label.out.log") -RedirectStandardError (Join-Path $run "$Label.err.log")
    try {
        if (-not $process.WaitForExit(180000)) { throw "Setup $Label timed out" }
        $process.WaitForExit()
        if ($process.ExitCode -notin @(0, 1641, 3010)) { throw "Setup $Label failed ($($process.ExitCode)); see $run" }
    } finally { $process.Dispose() }
}
function Assert-Owned {
    if (-not $owned -or -not (Test-Path -LiteralPath $marker)) { throw 'No installed qualification ownership' }
    $claim = Get-Content -LiteralPath $marker -Raw | ConvertFrom-Json
    if ($claim.nonce -ne $nonce -or $claim.sid -ne $sid -or $claim.root -ne $root -or $claim.fixture -ne 'installed-update') {
        throw 'Installed qualification ownership changed'
    }
    if (Test-Path -LiteralPath $registration) {
        $location = (Get-ItemProperty -LiteralPath $registration).InstallLocation
        if ([IO.Path]::GetFullPath($location).TrimEnd('\') -ne $root) { throw 'Compi registration belongs to another root' }
    }
    $task = Get-ScheduledTask -TaskName $taskName -TaskPath '\' -ErrorAction SilentlyContinue
    if ($task) {
        $owner = $task.Principal.UserId
        if ($owner -notlike 'S-1-*') { $owner = ([Security.Principal.NTAccount]$owner).Translate([Security.Principal.SecurityIdentifier]).Value }
        $actions = @($task.Actions)
        if ($owner -ne $sid -or $actions.Count -ne 1 -or $actions[0].Arguments -ne '--supervise') { throw 'Task is not owned by this account/qualification' }
        $action = $actions[0].Execute.Trim('"')
        if ($action.StartsWith('\\?\')) { throw 'Task action must use an ordinary drive path' }
        if ($action -notmatch ('^' + [regex]::Escape($root) + '\\versions\\\d+\.\d+\.\d+\\compi-daemon\.exe$')) {
            throw 'Task action belongs to another root or payload'
        }
    }
}
function Read-Installed([string]$ExpectedVersion, [string]$Label) {
    Assert-Owned
    $selection = Get-Content -LiteralPath (Join-Path $root 'selection.json') -Raw | ConvertFrom-Json
    $task = Get-ScheduledTask -TaskName $taskName -TaskPath '\'
    $expectedDaemon = Join-Path $root "versions\$ExpectedVersion\compi-daemon.exe"
    if ($selection.version -ne $ExpectedVersion -or $selection.task_version -ne $ExpectedVersion -or
        $task.Actions.Execute.Trim('"') -ne $expectedDaemon) { throw "Incorrect installed selection/task for $Label" }
    $value = @{selection = $selection; task_name = $taskName; account_sid = $sid; task_owner = $task.Principal.UserId;
               task_executable = $task.Actions.Execute; task_arguments = $task.Actions.Arguments;
               stable_helper_sha256 = (Hash (Join-Path $root 'compi-update-worker.exe'));
               registration = (Get-ItemProperty -LiteralPath $registration | Select-Object InstallLocation, DisplayVersion, UninstallString)}
    Write-Json (Join-Path $run "$Label.json") $value
    Export-ScheduledTask -TaskName $taskName -TaskPath '\' | Set-Content -LiteralPath (Join-Path $run "$Label-task.xml") -Encoding utf8
    return $value
}
function Get-OwnedProcesses {
    $processes = @(Get-CimInstance Win32_Process | Where-Object {
        $_.Name -in @('compi.exe', 'compi-daemon.exe', 'compi-update-worker.exe') -and
        $_.ExecutablePath -and $_.ExecutablePath.StartsWith($root + '\', [StringComparison]::OrdinalIgnoreCase)
    })
    foreach ($process in $processes) {
        $owner = Invoke-CimMethod -InputObject $process -MethodName GetOwnerSid
        if ($owner.ReturnValue -ne 0 -or $owner.Sid -ne $sid) { throw 'An installed-root process belongs to another account; refusing mutation' }
    }
    return $processes
}
function Stop-OwnedDefault {
    Assert-Owned
    $task = Get-ScheduledTask -TaskName $taskName -TaskPath '\' -ErrorAction SilentlyContinue
    if ($task) { Disable-ScheduledTask -InputObject $task | Out-Null }
    # The fresh account and marker own every process under this exact install root.
    # Never select by name alone or touch another account's processes.
    $processes = @(Get-OwnedProcesses | Where-Object { $_.Name -eq 'compi-daemon.exe' -and $_.CommandLine -notmatch '--instance(?:\s|=)' })
    if ($processes.Count) {
        if (-not $probe) { throw 'Cannot safely stop installed default daemon without existing-only probe' }
        & $probe --existing --instance '' workspace | Set-Content -LiteralPath (Join-Path $run 'default-before-stop.json')
        if ($LASTEXITCODE -ne 0) { throw 'Owned default processes exist without an inspectable endpoint' }
        $workspace = Get-Content -LiteralPath (Join-Path $run 'default-before-stop.json') -Raw | ConvertFrom-Json
        if (@($workspace.surfaces).Count -ne 0) { throw 'Refusing to stop default daemon with live shell work' }
        & $probe --existing --instance '' shutdown
        if ($LASTEXITCODE -ne 0) { throw 'Owned default shutdown failed' }
    }
    Wait-Condition -Description 'owned idle default daemon and supervisor exit' -Condition {
        $remaining = @(Get-OwnedProcesses | Where-Object { $_.Name -eq 'compi-daemon.exe' -and $_.CommandLine -notmatch '--instance(?:\s|=)' })
        return $remaining.Count -eq 0
    }
    if ($task) { Enable-ScheduledTask -TaskName $taskName -TaskPath '\' | Out-Null }
}

try {
    if (-not $env:GPUI_FXC_PATH) {
        $sdkRoot = Join-Path ${env:ProgramFiles(x86)} 'Windows Kits\10\bin'
        $fxc = Get-ChildItem -Path $sdkRoot -Filter fxc.exe -File -Recurse |
            Where-Object { $_.FullName -match '\\x64\\fxc\.exe$' } |
            Sort-Object { [version]$_.Directory.Parent.Name } -Descending |
            Select-Object -First 1
        if (-not $fxc) { throw "fxc.exe was not found below $sdkRoot" }
        $env:GPUI_FXC_PATH = $fxc.FullName
    }
    # Copy only source inputs, including compile-time release notes and pinned runtime.
    foreach ($name in @('Cargo.toml', 'Cargo.lock', 'LICENSE', 'crates', 'assets', 'docs', '.cargo')) {
        Copy-Item -LiteralPath (Join-Path $projectRoot $name) -Destination $source -Recurse
    }
    New-Item -ItemType Directory -Path (Join-Path $source 'tools'), (Join-Path $source 'target\conpty-runtime') -Force | Out-Null
    Copy-Item -LiteralPath (Join-Path $projectRoot 'tools\compi-dev') -Destination (Join-Path $source 'tools') -Recurse
    Copy-Item -LiteralPath (Join-Path $projectRoot 'target\conpty-runtime\x64') -Destination (Join-Path $source 'target\conpty-runtime') -Recurse
    $env:CARGO_TARGET_DIR = $build
    Push-Location $source
    try {
        Invoke-Cargo -Arguments @('build', '--locked', '-p', 'compi-update', '--bin', 'compi-release-metadata') -Label 'build-keygen'
        $signer = Join-Path $build 'debug\compi-release-metadata.exe'
        $publicKey = (& $signer keygen --private-key $privateKey | Out-String).Trim()
        if ($LASTEXITCODE -ne 0 -or [Convert]::FromBase64String($publicKey).Length -ne 32) { throw 'Disposable key generation failed' }
        $env:COMPI_UPDATE_PUBLIC_KEY = $publicKey
        Invoke-Cargo -Arguments @('build', '--locked', '-p', 'compi-client', '-p', 'compi-daemon', '-p', 'compi-update', '--bins', '--examples') -Label 'build-fixture-baseline'
    } finally { Pop-Location }
    $bin = Join-Path $build 'debug'
    $probe = Join-Path $bin 'examples\compi-probe.exe'
    $driver = Join-Path $run 'compi-update-smoke.exe'
    Copy-Item -LiteralPath (Join-Path $bin 'examples\compi-update-smoke.exe') -Destination $driver
    # Authenticate the historical release with its published Ed25519 Setup envelope,
    # then require exact version, URL, byte length, and SHA before executing anything.
    $baselineMetadata = Join-Path $run 'official-0.3.0-setup-metadata.json'
    Invoke-WebRequest -Uri 'https://github.com/cloudboy-jh/compi/releases/download/v0.3.0/compi-setup-windows-x86_64.json' -OutFile $baselineMetadata
    & python -c 'import cryptography' 2>$null
    if ($LASTEXITCODE -ne 0) {
        & python -m pip install cryptography
        if ($LASTEXITCODE -ne 0) { throw 'Cannot obtain hosted-only Ed25519 verifier' }
    }
    $verify = @'
import base64,json,sys
from cryptography.hazmat.primitives.asymmetric.ed25519 import Ed25519PublicKey
signed=json.load(open(sys.argv[1],encoding='utf8'))
payload=base64.b64decode(signed['payload'],validate=True)
Ed25519PublicKey.from_public_bytes(base64.b64decode('FF3uB8q74EGMcZTKYccBS8h4G34OzPDlhPLmUz8dHqU=',validate=True)).verify(base64.b64decode(signed['signature'],validate=True),payload)
m=json.loads(payload)
assert m['schema']==1 and m['product']=='compi' and m['version']=='0.3.0' and m['platform']=='windows-x86_64'
assert m['setup']['url']=='https://github.com/cloudboy-jh/compi/releases/download/v0.3.0/Compi-0.3.0-Setup.exe'
print(json.dumps(m['setup']))
'@
    $verifiedSetup = & python -c $verify $baselineMetadata
    if ($LASTEXITCODE -ne 0) { throw 'Official 0.3.0 Setup signature/schema verification failed' }
    $baseline = $verifiedSetup | ConvertFrom-Json
    $baselineSetup = Join-Path $run 'Compi-0.3.0-Setup.exe'
    Invoke-WebRequest -Uri $baseline.url -OutFile $baselineSetup
    if ((Get-Item -LiteralPath $baselineSetup).Length -ne $baseline.size -or (Hash $baselineSetup) -ne $baseline.sha256) {
        throw 'Authenticated official 0.3.0 Setup size/SHA mismatch'
    }
    # Ownership is established before the first installer can partially mutate state.
    New-Item -ItemType Directory -Path (Split-Path $root -Parent) -Force | Out-Null
    if (Test-Path -LiteralPath $marker) { throw 'Existing ownership marker is not ours' }
    Write-Json $marker @{root = $root; sid = $sid; nonce = $nonce; fixture = 'installed-update'; source_setup_sha256 = (Hash $setup)}
    $owned = $true
    Invoke-Setup $baselineSetup 'install' 'official-0.3.0-install'
    $oldInstalled = Read-Installed '0.3.0' 'official-0.3.0-installed'
    # This qualification replaces an idle default generation, not live user work.
    # Stop only our inspected empty baseline daemon before the real Setup migration.
    Stop-OwnedDefault
    Invoke-Setup $setup 'install' 'official-current-migration'
    $migrated = Read-Installed $version 'official-current-installed'
    $officialPayload = Join-Path $run 'official-portable-payload'
    Expand-Archive -LiteralPath $portable -DestinationPath $officialPayload
    $expectedHelper = Join-Path $officialPayload 'compi-update-worker.exe'
    if ($migrated.stable_helper_sha256 -ne (Hash $expectedHelper) -or
        $migrated.stable_helper_sha256 -ne (Hash (Join-Path $root "versions\$version\compi-update-worker.exe"))) {
        throw 'Real Setup migration did not install the current packaged stable root helper'
    }
    Write-Json (Join-Path $run 'setup-migration-evidence.json') @{
        schema = 1; platform = 'windows-x86_64'; from_version = '0.3.0'; to_version = $version;
        official_metadata_sha256 = (Hash $baselineMetadata); historical_setup_sha256 = (Hash $baselineSetup);
        current_setup_sha256 = (Hash $setup); current_portable_sha256 = (Hash $portable);
        old_install = $oldInstalled; migrated_install = $migrated;
        observed = @('official-setup-ed25519-and-sha256-authenticated', 'real-setup-migration', 'stable-helper-matches-current-package')
    }
    Stop-OwnedDefault
    Assert-Owned
    # Only this disposable account's fixture is changed to the disposable trust key.
    # Do not modify distribution artifacts or replace the stable helper with C.
    foreach ($name in @('compi.exe', 'compi-daemon.exe', 'compi-update-worker.exe')) {
        Copy-Item -LiteralPath (Join-Path $bin $name) -Destination (Join-Path $root "versions\$version\$name") -Force
    }
    Copy-Item -LiteralPath (Join-Path $bin 'compi-update-worker.exe') -Destination (Join-Path $root 'compi-update-worker.exe') -Force
    $fixtureHelperHash = Hash (Join-Path $root 'compi-update-worker.exe')
    # Build a genuinely newer generation from the same repaired source; only the
    # copied workspace version changes. Lockfile resolution remains offline.
    $snapshotManifest = Join-Path $source 'Cargo.toml'
    $updatedManifest = $manifest.Remove($versionMatch.Groups[1].Index, $version.Length).Insert($versionMatch.Groups[1].Index, $futureVersion)
    [IO.File]::WriteAllText($snapshotManifest, $updatedManifest, $utf8)
    Push-Location $source
    try {
        Invoke-Cargo -Arguments @('build', '--offline', '-p', 'compi-client', '-p', 'compi-daemon', '-p', 'compi-update', '--bins') -Label 'build-fixture-future'
    } finally { Pop-Location }
    $candidateDirectory = Join-Path $run 'future-payload'
    New-Item -ItemType Directory -Path $candidateDirectory | Out-Null
    foreach ($name in @('compi.exe', 'compi-daemon.exe', 'compi-update-worker.exe', 'conpty.dll', 'OpenConsole.exe', 'ConPTY-LICENSE.txt')) {
        Copy-Item -LiteralPath (Join-Path $bin $name) -Destination $candidateDirectory
    }
    Copy-Item -LiteralPath (Join-Path $projectRoot 'LICENSE') -Destination $candidateDirectory
    $candidateArchive = Join-Path $run "Compi-$futureVersion-Windows-x64-update-fixture.zip"
    Compress-Archive -Path (Join-Path $candidateDirectory '*') -DestinationPath $candidateArchive
    & $signer sign --version $futureVersion --platform windows-x86_64 --artifact $candidateArchive `
        --url "https://github.com/cloudboy-jh/compi/releases/download/v$futureVersion/Compi-$futureVersion-Windows-x64-update.zip" `
        --output $run --daemon-protocol 32 --qualified-daemon $version --key-file $privateKey
    if ($LASTEXITCODE -ne 0) { throw 'Disposable candidate metadata signing failed' }
    $candidateMetadata = Join-Path $run 'compi-update-windows-x86_64.json'
    & python (Join-Path $PSScriptRoot 'smoke-update-cycle.py') --installed-fixture --root $root `
        --artifact $candidateArchive --metadata $candidateMetadata --probe $probe --driver $driver --evidence $run
    if ($LASTEXITCODE -ne 0) { throw 'Real installed in-app qualification failed' }
    $cycleReceipts = @(Get-ChildItem -LiteralPath $run -Directory -Filter 'cycle-*' | ForEach-Object { Join-Path $_.FullName 'evidence.json' })
    if ($cycleReceipts.Count -ne 1 -or -not (Test-Path -LiteralPath $cycleReceipts[0])) { throw 'Missing exact installed cycle receipt' }
    $cycle = Get-Content -LiteralPath $cycleReceipts[0] -Raw | ConvertFrom-Json
    if ($cycle.installation_kind -ne 'InstalledWindows' -or -not $cycle.fixture -or $cycle.account_sid -ne $sid -or
        $cycle.from_version -ne $version -or $cycle.to_version -ne $futureVersion -or
        $cycle.artifact_sha256 -ne (Hash $candidateArchive) -or $cycle.stable_helper_sha256 -ne $fixtureHelperHash) {
        throw 'Installed cycle receipt does not match the exact owned disposable fixture'
    }
    $finalInstalled = Read-Installed $futureVersion 'future-installed'
    if ((Hash (Join-Path $root 'compi-update-worker.exe')) -ne $fixtureHelperHash) { throw 'Update changed the stable root helper to the staged candidate' }
    Write-Json (Join-Path $run 'installed-qualification.json') @{
        schema = 1; platform = 'windows-x86_64'; version = $version; account_sid = $sid;
        setup_migration_receipt = 'setup-migration-evidence.json'; installed_fixture_cycle_receipt = $cycleReceipts[0];
        fixture_public_key = $publicKey; fixture_artifact_sha256 = (Hash $candidateArchive);
        fixture_stable_helper_sha256 = $fixtureHelperHash; final_install = $finalInstalled;
        update_state_diagnostics = 'update-state-before-uninstall';
        scope = 'Real official Setup migration plus disposable-key future installed compatible named-shell cycle; not production artifact signing or incompatible-shell preservation proof'
    }
} catch {
    $failure = $_
} finally {
    if ($owned) {
        try {
            Assert-Owned
            # Recover named cleanup even if Python failed during GUI handoff.
            # The nonce binds the random instance to this exact invocation.
            $claims = @(Get-ChildItem -LiteralPath $run -Directory -Filter 'cycle-*' | ForEach-Object {
                $claimPath = Join-Path $_.FullName 'ownership.json'
                if (Test-Path -LiteralPath $claimPath) { Get-Content -LiteralPath $claimPath -Raw | ConvertFrom-Json }
            })
            foreach ($claim in $claims) {
                if ($claim.root -ne $root -or $claim.sid -ne $sid -or $claim.nonce -ne $nonce -or $claim.instance -notmatch '^upd-[0-9a-f]{16}$') {
                    throw 'Named cleanup ownership mismatch'
                }
                $named = @(Get-OwnedProcesses | Where-Object { $_.Name -eq 'compi-daemon.exe' -and $_.CommandLine -match ('--instance ' + [regex]::Escape($claim.instance) + '(?:\s|$)') })
                if ($named.Count) {
                    & $probe --existing --instance $claim.instance shutdown
                    if ($LASTEXITCODE -ne 0) { throw 'Owned named qualification daemon cleanup failed' }
                }
            }
            foreach ($guiProcess in @(Get-OwnedProcesses | Where-Object { $_.Name -eq 'compi.exe' })) {
                Stop-Process -Id $guiProcess.ProcessId -ErrorAction Stop
            }
            Wait-Condition -Description 'owned named qualification daemon, GUI, and helper cleanup' -Condition {
                return @(Get-OwnedProcesses | Where-Object { $_.Name -in @('compi.exe', 'compi-update-worker.exe') -or ($_.Name -eq 'compi-daemon.exe' -and $_.CommandLine -match '--instance(?:\s|=)') }).Count -eq 0
            }
            Stop-OwnedDefault
            # Keep original journals/requests and actual GUI readiness receipts
            # before the real uninstaller removes its owned transaction directory.
            $state = Join-Path $root '.compi-update'
            if (Test-Path -LiteralPath $state) {
                foreach ($file in @(Get-ChildItem -LiteralPath $state -Recurse -File | Where-Object { $_.Extension -in @('.json', '.log', '.xml') })) {
                    $relative = $file.FullName.Substring($state.Length + 1)
                    $destination = Join-Path (Join-Path $run 'update-state-before-uninstall') $relative
                    New-Item -ItemType Directory -Path (Split-Path $destination -Parent) -Force | Out-Null
                    Copy-Item -LiteralPath $file.FullName -Destination $destination
                }
            }
            if (Test-Path -LiteralPath $registration) { Invoke-Setup (Join-Path $root 'Compi-Setup.exe') 'remove' 'owned-uninstall' }
            Wait-Condition -Seconds 15 -Description 'owned uninstall files, registration, and SID task removal' -Condition {
                return -not ((Test-Path -LiteralPath $root) -or (Test-Path -LiteralPath $registration) -or
                    (Get-ScheduledTask -TaskName $taskName -TaskPath '\' -ErrorAction SilentlyContinue))
            }
            Remove-Item -LiteralPath $marker
        } catch { $cleanupErrors.Add($_.ToString()) }
        try {
            if (Test-Path -LiteralPath $data) { Copy-Item -LiteralPath $data -Destination (Join-Path $run 'account-data-logs') -Recurse }
        } catch { $cleanupErrors.Add($_.ToString()) }
    }
    $env:COMPI_UPDATE_PUBLIC_KEY = $originalKey
    $env:CARGO_TARGET_DIR = $originalTarget
    $env:GPUI_FXC_PATH = $originalFxc
    try { if (Test-Path -LiteralPath $scaffold) { Remove-Item -LiteralPath $scaffold -Recurse -Force } }
    catch { $cleanupErrors.Add($_.ToString()) }
    Write-Json (Join-Path $run 'cleanup.json') @{schema = 1; account_sid = $sid; errors = $cleanupErrors.ToArray(); operation_error = $(if ($failure) { $failure.ToString() } else { $null })}
}
if ($failure -or $cleanupErrors.Count) {
    throw "Installed qualification failed. Operation: $failure; cleanup: $($cleanupErrors -join '; '); evidence: $run"
}
Write-Host "PASS installed Setup migration and future in-app task replacement/rollback: $run\installed-qualification.json"
