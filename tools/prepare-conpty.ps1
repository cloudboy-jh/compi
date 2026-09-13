#requires -Version 7.0
[CmdletBinding()]
param(
    [ValidateSet('x64', 'arm64')]
    [string]$Architecture = 'x64'
)

$ErrorActionPreference = 'Stop'
if (-not $IsWindows) {
    throw 'ConPTY preparation requires Windows and PowerShell 7 (pwsh) to verify Microsoft Authenticode signatures.'
}

# Official Microsoft NuGet release; never resolve a floating version or use Windows' stock ConPTY.
# Provenance: https://api.nuget.org/v3/catalog0/data/2026.07.13.20.50.27/microsoft.windows.console.conpty.1.24.260710001.json
# Archive SHA512 matches that catalog. `dotnet nuget verify <archive> --all` verified both signatures:
# Microsoft author certificate SHA256: 566A31882BE208BE4422F7CFD66ED09F5D4524A5994F50CCC8B05EC0528C1353
# NuGet repository certificate SHA256: 1F4B311D9ACC115C8DC8018B5A49E00FCE6DA8E2855F9F014CA6F34570BC482D
# The SHA256 pin below fixes those verified bytes, including the package signatures.
$version = '1.24.260710001'
$archiveSha256 = '175640566a3b59c4b132070ee96c2c77e5ab7edd2e92732a5eb3610bbf63d90e'
$archiveName = "microsoft.windows.console.conpty.$version.nupkg"
$packageUrl = "https://api.nuget.org/v3-flatcontainer/microsoft.windows.console.conpty/$version/$archiveName"
$projectRoot = (Resolve-Path (Join-Path $PSScriptRoot '..')).Path
$runtimeRoot = Join-Path $projectRoot 'target\conpty-runtime'
$cacheRoot = Join-Path $runtimeRoot 'cache'
$archivePath = Join-Path $cacheRoot $archiveName
$destination = Join-Path $runtimeRoot $Architecture

# The package declares MIT in its nuspec but contains no license text. Preserve the upstream notice:
# https://github.com/microsoft/terminal/blob/d4d59fa3395012ca37ba12665da4ec11c7dcf9cb/LICENSE
$license = @'
Copyright (c) Microsoft Corporation. All rights reserved.

MIT License

Permission is hereby granted, free of charge, to any person obtaining a copy
of this software and associated documentation files (the "Software"), to deal
in the Software without restriction, including without limitation the rights
to use, copy, modify, merge, publish, distribute, sublicense, and/or sell
copies of the Software, and to permit persons to whom the Software is
furnished to do so, subject to the following conditions:

The above copyright notice and this permission notice shall be included in all
copies or substantial portions of the Software.

THE SOFTWARE IS PROVIDED *AS IS*, WITHOUT WARRANTY OF ANY KIND, EXPRESS OR
IMPLIED, INCLUDING BUT NOT LIMITED TO THE WARRANTIES OF MERCHANTABILITY,
FITNESS FOR A PARTICULAR PURPOSE AND NONINFRINGEMENT. IN NO EVENT SHALL THE
AUTHORS OR COPYRIGHT HOLDERS BE LIABLE FOR ANY CLAIM, DAMAGES OR OTHER
LIABILITY, WHETHER IN AN ACTION OF CONTRACT, TORT OR OTHERWISE, ARISING FROM,
OUT OF OR IN CONNECTION WITH THE SOFTWARE OR THE USE OR OTHER DEALINGS IN THE
SOFTWARE.
'@

function Assert-ArchiveHash {
    param([Parameter(Mandatory)] [string]$Path)
    $actual = (Get-FileHash -LiteralPath $Path -Algorithm SHA256).Hash
    if ($actual -ne $archiveSha256) {
        throw "ConPTY archive SHA256 mismatch at '$Path': expected $archiveSha256, got $actual. Remove the rejected archive and rerun pwsh -File tools/prepare-conpty.ps1; do not substitute a different package or system binary."
    }
}

New-Item -ItemType Directory -Force -Path $cacheRoot | Out-Null
$lock = $null
$downloadPath = $null
$staging = $null
$backup = $null
try {
    try {
        $lock = [System.IO.File]::Open((Join-Path $runtimeRoot 'prepare.lock'), 'OpenOrCreate', 'ReadWrite', 'None')
    }
    catch {
        throw "Cannot lock ConPTY preparation at '$runtimeRoot'. Wait for any other preparation to finish, check directory permissions, and rerun. $($_.Exception.Message)"
    }

    if (-not (Test-Path -LiteralPath $archivePath -PathType Leaf)) {
        $downloadPath = Join-Path $cacheRoot "$archiveName.$([guid]::NewGuid().ToString('N')).download"
        Write-Host "Downloading Microsoft ConPTY $version from $packageUrl"
        Invoke-WebRequest -Uri $packageUrl -OutFile $downloadPath -TimeoutSec 120 -MaximumRetryCount 2 -RetryIntervalSec 2
        Assert-ArchiveHash $downloadPath
        [System.IO.File]::Move($downloadPath, $archivePath)
        $downloadPath = $null
    }
    Assert-ArchiveHash $archivePath

    $staging = Join-Path $runtimeRoot ".$Architecture.$([guid]::NewGuid().ToString('N')).staging"
    New-Item -ItemType Directory -Path $staging | Out-Null
    $entries = [ordered]@{
        'conpty.dll' = "runtimes/win-$Architecture/native/conpty.dll"
        'OpenConsole.exe' = "build/native/runtimes/$Architecture/OpenConsole.exe"
    }
    $archive = [System.IO.Compression.ZipFile]::OpenRead($archivePath)
    try {
        foreach ($name in $entries.Keys) {
            $matches = @($archive.Entries | Where-Object { $_.FullName -ceq $entries[$name] })
            if ($matches.Count -ne 1 -or $matches[0].Length -eq 0) {
                throw "Pinned ConPTY package must contain exactly one nonempty '$($entries[$name])'. Remove '$archivePath' and rerun preparation."
            }
            $path = Join-Path $staging $name
            [System.IO.Compression.ZipFileExtensions]::ExtractToFile($matches[0], $path)
            $signature = Get-AuthenticodeSignature -LiteralPath $path
            if ($signature.Status -ne 'Valid' -or $signature.SignerCertificate.Subject -notmatch '(^|, )O=Microsoft Corporation(,|$)') {
                throw "Microsoft Authenticode verification failed for '$name' ($Architecture): $($signature.Status), $($signature.StatusMessage). Check Windows certificate trust/network access and rerun preparation; no unsigned/system fallback is permitted."
            }
        }
    }
    finally {
        $archive.Dispose()
    }
    [System.IO.File]::WriteAllText((Join-Path $staging 'ConPTY-LICENSE.txt'), ($license.Replace("`r`n", "`n") + "`n"), [System.Text.UTF8Encoding]::new($false))

    # Leave an already verified matching directory untouched so Cargo's file tracking stays stable.
    $unchanged = Test-Path -LiteralPath $destination -PathType Container
    foreach ($name in @('conpty.dll', 'OpenConsole.exe', 'ConPTY-LICENSE.txt')) {
        $installed = Join-Path $destination $name
        if (-not (Test-Path -LiteralPath $installed -PathType Leaf) -or
            (Get-FileHash -LiteralPath $installed -Algorithm SHA256).Hash -ne
            (Get-FileHash -LiteralPath (Join-Path $staging $name) -Algorithm SHA256).Hash) {
            $unchanged = $false
        }
    }
    if ($unchanged) {
        Write-Host "Microsoft ConPTY $version ($Architecture) is verified and ready at $destination"
        return
    }

    # Publish only a complete verified pair/license, by same-volume directory rename. Windows cannot
    # replace a nonempty directory directly; retain the previous directory for rollback until publication.
    if (Test-Path -LiteralPath $destination) {
        $backup = Join-Path $runtimeRoot ".$Architecture.$([guid]::NewGuid().ToString('N')).previous"
        [System.IO.Directory]::Move($destination, $backup)
    }
    try {
        [System.IO.Directory]::Move($staging, $destination)
        $staging = $null
    }
    catch {
        if ($backup) {
            [System.IO.Directory]::Move($backup, $destination)
            $backup = $null
        }
        throw
    }
    if ($backup) {
        Remove-Item -LiteralPath $backup -Recurse -Force
        $backup = $null
    }
    Write-Host "Prepared Microsoft ConPTY $version ($Architecture) at $destination"
}
catch {
    throw "ConPTY runtime preparation failed: $($_.Exception.Message) Run pwsh -File tools/prepare-conpty.ps1 -Architecture $Architecture successfully before building/running the Windows daemon."
}
finally {
    foreach ($path in @($downloadPath, $staging)) {
        if ($path -and (Test-Path -LiteralPath $path)) {
            Remove-Item -LiteralPath $path -Recurse -Force
        }
    }
    if ($lock) { $lock.Dispose() }
}
