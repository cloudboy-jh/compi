[CmdletBinding()]
param(
    [string]$OutputDirectory,
    [string]$SigningCertificateThumbprint,
    [string]$TimestampUrl = 'http://timestamp.digicert.com',
    [string]$ExpectedTag,
    [switch]$RequireSigning,
    [switch]$RequireUpdateKey
)

$ErrorActionPreference = 'Stop'
$projectRoot = (Resolve-Path (Join-Path $PSScriptRoot '..')).Path
if (-not $OutputDirectory) {
    $OutputDirectory = Join-Path $projectRoot 'target\distribution'
}
$manifest = Get-Content -Raw (Join-Path $projectRoot 'Cargo.toml')
if ($manifest -notmatch '(?ms)^\[workspace\.package\](?:(?!^\[).)*?^version\s*=\s*"([^"]+)"') {
    throw 'Could not read the Compi workspace version from Cargo.toml'
}
$version = $Matches[1]
$bootstrapperManifest = Get-Content -Raw (Join-Path $projectRoot 'installer\bootstrapper\Cargo.toml')
if ($bootstrapperManifest -notmatch '(?ms)^\[package\].*?^version\s*=\s*"([^"]+)"' -or $Matches[1] -ne $version) {
    throw "installer/bootstrapper/Cargo.toml must use Compi version $version"
}
if ($ExpectedTag -and $ExpectedTag -ne "v$version") {
    throw "Release tag $ExpectedTag does not match Cargo workspace version $version"
}
if ($RequireSigning -and -not $SigningCertificateThumbprint) {
    throw 'A signing certificate thumbprint is required for this release build'
}
if ($RequireUpdateKey -and -not $env:COMPI_UPDATE_PUBLIC_KEY) {
    throw 'Trusted public updates require COMPI_UPDATE_PUBLIC_KEY at compile time'
}
if (-not $env:COMPI_UPDATE_PUBLIC_KEY) {
    Write-Warning 'Dogfood build: public updater disabled (no verification key configured)'
}
if ($env:COMPI_UPDATE_PUBLIC_KEY) {
    try { $updateKey = [Convert]::FromBase64String($env:COMPI_UPDATE_PUBLIC_KEY) }
    catch { throw 'COMPI_UPDATE_PUBLIC_KEY must be valid base64' }
    if ($updateKey.Length -ne 32) { throw 'COMPI_UPDATE_PUBLIC_KEY must encode a 32-byte Ed25519 public key' }
}
$sdkRoot = Join-Path ${env:ProgramFiles(x86)} 'Windows Kits\10\bin'
$signTool = $null
if ($SigningCertificateThumbprint) {
    $signTool = Get-ChildItem -Path $sdkRoot -Filter signtool.exe -File -Recurse |
        Where-Object { $_.FullName -match '\\x64\\signtool\.exe$' } |
        Sort-Object { [version]$_.Directory.Parent.Name } -Descending |
        Select-Object -First 1
    if (-not $signTool) {
        throw "signtool.exe was not found below $sdkRoot"
    }
}

function Invoke-SignArtifact {
    param([Parameter(Mandatory)] [string]$Path)

    if (-not $script:signTool) {
        return
    }
    & $script:signTool.FullName sign /sha1 $SigningCertificateThumbprint /fd SHA256 /tr $TimestampUrl /td SHA256 $Path
    if ($LASTEXITCODE -ne 0) { throw "Failed to sign $Path" }
    & $script:signTool.FullName verify /pa $Path
    if ($LASTEXITCODE -ne 0) { throw "Signature verification failed for $Path" }
}

function Assert-FileVersion {
    param([Parameter(Mandatory)] [string]$Path)

    $actual = (Get-Item $Path).VersionInfo.ProductVersion
    if (-not $actual -or -not $actual.StartsWith($script:version, [System.StringComparison]::Ordinal)) {
        throw "$Path has product version '$actual'; expected $script:version"
    }
}

function Get-PayloadComponentGuid {
    param([Parameter(Mandatory)] [string]$Name)

    # A component GUID must change when its versioned installation directory changes.
    $sha256 = [System.Security.Cryptography.SHA256]::Create()
    try {
        $hash = $sha256.ComputeHash([System.Text.Encoding]::UTF8.GetBytes("compi/payload/$script:version/$Name"))
        return [guid]::new([byte[]]$hash[0..15]).ToString('B').ToUpperInvariant()
    }
    finally {
        $sha256.Dispose()
    }
}

$installerRoot = Join-Path $projectRoot 'target\installer'
$productTarget = Join-Path $installerRoot 'product'
$productBin = Join-Path $productTarget 'release'
$msiPath = Join-Path $installerRoot 'Compi.msi'
$payloadDirectory = Join-Path $projectRoot 'installer\bootstrapper\payload'
$payloadPath = Join-Path $payloadDirectory 'Compi.msi'
$maintenanceTarget = Join-Path $installerRoot 'maintenance-target'
$maintenanceSource = Join-Path $maintenanceTarget 'release\compi-maintenance.exe'
$bootstrapperTarget = Join-Path $installerRoot 'bootstrapper-target'
$setupSource = Join-Path $bootstrapperTarget 'release\compi-setup.exe'
$setupName = "Compi-$version-Setup.exe"
$setupDestination = Join-Path $OutputDirectory $setupName
$portableName = "Compi-$version-Windows-x64.zip"
$portableDestination = Join-Path $OutputDirectory $portableName
$updateName = "Compi-$version-Windows-x64-update.zip"
$updateDestination = Join-Path $OutputDirectory $updateName
$launcherSource = Join-Path $productBin 'compi-launcher.exe'
$helperSource = Join-Path $productBin 'compi-update-worker.exe'
$portableStaging = Join-Path $installerRoot ('portable-' + [guid]::NewGuid().ToString('N'))

if (-not $env:GPUI_FXC_PATH) {
    $fxc = Get-ChildItem -Path $sdkRoot -Filter fxc.exe -File -Recurse |
        Where-Object { $_.FullName -match '\\x64\\fxc\.exe$' } |
        Sort-Object { [version]$_.Directory.Parent.Name } -Descending |
        Select-Object -First 1
    if (-not $fxc) {
        throw "fxc.exe was not found below $sdkRoot"
    }
    $env:GPUI_FXC_PATH = $fxc.FullName
}

New-Item -ItemType Directory -Force -Path $installerRoot, $payloadDirectory, $OutputDirectory | Out-Null
Push-Location $projectRoot
try {
    & dotnet tool restore
    if ($LASTEXITCODE -ne 0) { throw 'Failed to restore the pinned WiX tool' }

    & (Join-Path $PSScriptRoot 'prepare-conpty.ps1') -Architecture x64

    & cargo build --locked --release -p compi-client -p compi-daemon -p compi-update --bins --target-dir $productTarget
    if ($LASTEXITCODE -ne 0) { throw 'Failed to build Compi product binaries' }
    Assert-FileVersion (Join-Path $productBin 'compi.exe')
    Assert-FileVersion (Join-Path $productBin 'compi-daemon.exe')
    Assert-FileVersion $launcherSource
    Assert-FileVersion $helperSource
    foreach ($runtimeFile in @('conpty.dll', 'OpenConsole.exe', 'ConPTY-LICENSE.txt')) {
        if (-not (Test-Path -LiteralPath (Join-Path $productBin $runtimeFile) -PathType Leaf)) {
            throw "Missing bundled runtime '$runtimeFile' in '$productBin'. Rerun tools/prepare-conpty.ps1 and rebuild the daemon before packaging."
        }
    }
    Invoke-SignArtifact (Join-Path $productBin 'compi.exe')
    Invoke-SignArtifact (Join-Path $productBin 'compi-daemon.exe')
    Invoke-SignArtifact $launcherSource
    Invoke-SignArtifact $helperSource
    & cargo build --locked --manifest-path installer\bootstrapper\Cargo.toml --release --bin compi-maintenance --target-dir $maintenanceTarget
    if ($LASTEXITCODE -ne 0) { throw 'Failed to build the installed Compi maintenance surface' }
    Assert-FileVersion $maintenanceSource
    Invoke-SignArtifact $maintenanceSource


    & dotnet tool run wix build installer\Compi.wxs -arch x64 `
        -d "Version=$version" `
        -d "BinDir=$productBin" `
        -d "ProjectDir=$projectRoot" `
        -d "MaintenanceExe=$maintenanceSource" `
        -d "LauncherExe=$launcherSource" `
        -d "HelperExe=$helperSource" `
        -d "PayloadClientGuid=$(Get-PayloadComponentGuid 'Client')" `
        -d "PayloadDaemonGuid=$(Get-PayloadComponentGuid 'Daemon')" `
        -d "PayloadWorkerGuid=$(Get-PayloadComponentGuid 'Worker')" `
        -d "PayloadConptyGuid=$(Get-PayloadComponentGuid 'Conpty')" `
        -d "PayloadOpenConsoleGuid=$(Get-PayloadComponentGuid 'OpenConsole')" `
        -d "PayloadConptyLicenseGuid=$(Get-PayloadComponentGuid 'ConptyLicense')" `
        -d "PayloadLicenseGuid=$(Get-PayloadComponentGuid 'License')" `
        -o $msiPath
    if ($LASTEXITCODE -ne 0) { throw 'Failed to build Compi.msi' }

    & dotnet tool run wix msi validate $msiPath
    if ($LASTEXITCODE -ne 0) { throw 'Compi.msi failed Windows Installer validation' }
    Invoke-SignArtifact $msiPath

    Copy-Item -Force $msiPath $payloadPath
    & cargo build --locked --manifest-path installer\bootstrapper\Cargo.toml --release --bin compi-setup --target-dir $bootstrapperTarget
    if ($LASTEXITCODE -ne 0) { throw 'Failed to build the Compi Setup bootstrapper' }
    Assert-FileVersion $setupSource
    Invoke-SignArtifact $setupSource

    Copy-Item -Force $setupSource $setupDestination
    # releases/latest/download/Compi-Setup.exe always names the newest Setup; same bytes.
    $stableSetupDestination = Join-Path $OutputDirectory 'Compi-Setup.exe'
    Copy-Item -Force $setupDestination $stableSetupDestination

    New-Item -ItemType Directory -Force -Path $portableStaging | Out-Null
    $versionDirectory = Join-Path $portableStaging "versions\$version"
    New-Item -ItemType Directory -Force -Path $versionDirectory | Out-Null
    foreach ($file in @('compi.exe', 'compi-daemon.exe', 'compi-update-worker.exe', 'conpty.dll', 'OpenConsole.exe', 'ConPTY-LICENSE.txt')) {
        Copy-Item -LiteralPath (Join-Path $productBin $file) -Destination $versionDirectory
    }
    Copy-Item -LiteralPath (Join-Path $projectRoot 'LICENSE') -Destination $versionDirectory
    Copy-Item -LiteralPath $launcherSource -Destination (Join-Path $portableStaging 'compi.exe')
    Copy-Item -LiteralPath $helperSource -Destination $portableStaging
    Copy-Item -LiteralPath (Join-Path $projectRoot 'LICENSE') -Destination $portableStaging
    [System.IO.File]::WriteAllText((Join-Path $portableStaging 'selection.json'),
        (@{schema = 1; version = $version; task_version = $version} | ConvertTo-Json -Compress),
        [System.Text.UTF8Encoding]::new($false))
    # The updater consumes the complete payload, not the stable launch wrapper.
    foreach ($destination in @($portableDestination, $updateDestination)) {
        if (Test-Path -LiteralPath $destination) { Remove-Item -LiteralPath $destination -Force }
    }
    Compress-Archive -Path (Join-Path $portableStaging '*') -DestinationPath $portableDestination
    Compress-Archive -Path (Join-Path $versionDirectory '*') -DestinationPath $updateDestination

    $checksumPath = Join-Path $OutputDirectory 'SHA256SUMS.txt'
    $checksums = foreach ($artifact in @($setupDestination, $stableSetupDestination, $portableDestination, $updateDestination)) {
        $sha256 = [System.Security.Cryptography.SHA256]::Create()
        $stream = [System.IO.File]::OpenRead($artifact)
        try {
            $hashBytes = $sha256.ComputeHash($stream)
            $hash = ([System.BitConverter]::ToString($hashBytes)).Replace('-', '').ToLowerInvariant()
        }
        finally {
            $stream.Dispose()
            $sha256.Dispose()
        }
        "$hash *$([System.IO.Path]::GetFileName($artifact))"
    }
    [System.IO.File]::WriteAllText(
        $checksumPath,
        ($checksums -join "`n") + "`n",
        [System.Text.ASCIIEncoding]::new()
    )

    Write-Host "Setup: $setupDestination"
    Write-Host "Portable: $portableDestination"
    Write-Host "Update payload: $updateDestination"
    Write-Host "Checksums: $checksumPath"
}
finally {
    Pop-Location
    Remove-Item -Force -ErrorAction SilentlyContinue $payloadPath
    if (Test-Path -LiteralPath $portableStaging) { Remove-Item -LiteralPath $portableStaging -Recurse -Force }
}
