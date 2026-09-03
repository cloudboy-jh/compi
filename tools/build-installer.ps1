[CmdletBinding()]
param(
    [string]$OutputDirectory,
    [string]$SigningCertificateThumbprint,
    [string]$TimestampUrl = 'http://timestamp.digicert.com',
    [string]$ExpectedTag,
    [switch]$RequireSigning
)

$ErrorActionPreference = 'Stop'
$projectRoot = (Resolve-Path (Join-Path $PSScriptRoot '..')).Path
if (-not $OutputDirectory) {
    $OutputDirectory = Join-Path $projectRoot 'target\distribution'
}
$manifest = Get-Content -Raw (Join-Path $projectRoot 'Cargo.toml')
if ($manifest -notmatch '(?ms)^\[package\].*?^version\s*=\s*"([^"]+)"') {
    throw 'Could not read the Compi version from Cargo.toml'
}
$version = $Matches[1]
$bootstrapperManifest = Get-Content -Raw (Join-Path $projectRoot 'installer\bootstrapper\Cargo.toml')
if ($bootstrapperManifest -notmatch '(?ms)^\[package\].*?^version\s*=\s*"([^"]+)"' -or $Matches[1] -ne $version) {
    throw "installer/bootstrapper/Cargo.toml must use Compi version $version"
}
if ($ExpectedTag -and $ExpectedTag -ne "v$version") {
    throw "Release tag $ExpectedTag does not match Cargo package version $version"
}
if ($RequireSigning -and -not $SigningCertificateThumbprint) {
    throw 'A signing certificate thumbprint is required for this release build'
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

    & cargo build --release --bin compi --bin compi-daemon --target-dir $productTarget
    if ($LASTEXITCODE -ne 0) { throw 'Failed to build Compi product binaries' }
    Assert-FileVersion (Join-Path $productBin 'compi.exe')
    Assert-FileVersion (Join-Path $productBin 'compi-daemon.exe')
    Invoke-SignArtifact (Join-Path $productBin 'compi.exe')
    Invoke-SignArtifact (Join-Path $productBin 'compi-daemon.exe')
    & cargo build --manifest-path installer\bootstrapper\Cargo.toml --release --bin compi-maintenance --target-dir $maintenanceTarget
    if ($LASTEXITCODE -ne 0) { throw 'Failed to build the installed Compi maintenance surface' }
    Assert-FileVersion $maintenanceSource
    Invoke-SignArtifact $maintenanceSource


    & dotnet tool run wix build installer\Compi.wxs -arch x64 `
        -d "Version=$version" `
        -d "BinDir=$productBin" `
        -d "ProjectDir=$projectRoot" `
        -d "MaintenanceExe=$maintenanceSource" `
        -o $msiPath
    if ($LASTEXITCODE -ne 0) { throw 'Failed to build Compi.msi' }

    & dotnet tool run wix msi validate $msiPath
    if ($LASTEXITCODE -ne 0) { throw 'Compi.msi failed Windows Installer validation' }
    Invoke-SignArtifact $msiPath

    Copy-Item -Force $msiPath $payloadPath
    & cargo build --manifest-path installer\bootstrapper\Cargo.toml --release --bin compi-setup --target-dir $bootstrapperTarget
    if ($LASTEXITCODE -ne 0) { throw 'Failed to build the Compi Setup bootstrapper' }
    Assert-FileVersion $setupSource
    Invoke-SignArtifact $setupSource

    Copy-Item -Force $setupSource $setupDestination

    if (Test-Path $portableDestination) {
        Remove-Item -Force $portableDestination
    }
    Compress-Archive -Path @(
        (Join-Path $productBin 'compi.exe'),
        (Join-Path $productBin 'compi-daemon.exe')
    ) -DestinationPath $portableDestination

    $checksumPath = Join-Path $OutputDirectory 'SHA256SUMS.txt'
    $checksums = foreach ($artifact in @($setupDestination, $portableDestination)) {
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
    Write-Host "Checksums: $checksumPath"
}
finally {
    Pop-Location
    Remove-Item -Force -ErrorAction SilentlyContinue $payloadPath
}
