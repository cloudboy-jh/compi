[CmdletBinding()]
param(
    [string]$OutputDirectory
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
    $sdkRoot = Join-Path ${env:ProgramFiles(x86)} 'Windows Kits\10\bin'
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
    & cargo build --manifest-path installer\bootstrapper\Cargo.toml --release --bin compi-maintenance --target-dir $maintenanceTarget
    if ($LASTEXITCODE -ne 0) { throw 'Failed to build the installed Compi maintenance surface' }


    & dotnet tool run wix build installer\Compi.wxs -arch x64 `
        -d "Version=$version" `
        -d "BinDir=$productBin" `
        -d "ProjectDir=$projectRoot" `
        -d "MaintenanceExe=$maintenanceSource" `
        -o $msiPath
    if ($LASTEXITCODE -ne 0) { throw 'Failed to build Compi.msi' }

    & dotnet tool run wix msi validate $msiPath
    if ($LASTEXITCODE -ne 0) { throw 'Compi.msi failed Windows Installer validation' }

    Copy-Item -Force $msiPath $payloadPath
    & cargo build --manifest-path installer\bootstrapper\Cargo.toml --release --bin compi-setup --target-dir $bootstrapperTarget
    if ($LASTEXITCODE -ne 0) { throw 'Failed to build the Compi Setup bootstrapper' }

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
    Set-Content -Path $checksumPath -Value $checksums -Encoding ascii

    Write-Host "Setup: $setupDestination"
    Write-Host "Portable: $portableDestination"
    Write-Host "Checksums: $checksumPath"
}
finally {
    Pop-Location
    Remove-Item -Force -ErrorAction SilentlyContinue $payloadPath
}
