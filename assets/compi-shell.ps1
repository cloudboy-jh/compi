# Compi's shell integration for Windows PowerShell. Compi runs it with -NoExit after your
# profile, so it wraps whatever prompt your profile set; your startup files are not edited.
# Each prompt reports the shell's process id (once), its folder (OSC 7) and a new prompt
# (OSC 133 A, aid = this shell), then draws your prompt with $? and $LASTEXITCODE as your
# last command left them. A prompt chosen in Compi's prompt settings loads from
# ~/.compi/prompt and follows later changes at the next prompt.
if ($env:COMPI_CLI) {
    $env:PATH = [IO.Path]::GetDirectoryName($env:COMPI_CLI) + ';' + $env:PATH
}
$global:__compi_prompt_dir = Join-Path $HOME '.compi\prompt'
# Your profile's prompt, which turning Compi's prompt off returns to, with the provider
# variables it renders from (Oh My Posh keeps its configuration by POSH_SESSION_ID).
$global:__compi_prompt_original = $function:prompt
$global:__compi_prompt_variables = @(Get-ChildItem env: |
    Where-Object { $_.Name -like 'POSH_*' -or $_.Name -like 'STARSHIP_*' })
function global:__compi_prompt_load([string] $name) {
    $path = Join-Path $global:__compi_prompt_dir $name
    if (Test-Path -LiteralPath $path) {
        # A script block is not held to the script execution policy the way a file is.
        . ([ScriptBlock]::Create([IO.File]::ReadAllText($path)))
    }
}
function global:__compi_prompt_token {
    $path = Join-Path $global:__compi_prompt_dir 'reload'
    if (Test-Path -LiteralPath $path) { [IO.File]::ReadAllText($path).Trim() } else { '' }
}
$global:__compi_prompt_seen = __compi_prompt_token
__compi_prompt_load 'compi.ps1'
$global:__compi_prompt = $function:prompt
$global:__compi_pid_reported = $false
# Prompt settings changed: back to your prompt, then load what is applied now. This runs
# while PowerShell waits for input, since a provider that replaces its own module cannot
# do so from inside the prompt function.
$null = Register-EngineEvent -SourceIdentifier PowerShell.OnIdle -Action {
    $token = __compi_prompt_token
    if ($token -ne $global:__compi_prompt_seen) {
        $global:__compi_prompt_seen = $token
        $wrapper = $function:global:prompt
        Get-ChildItem env: |
            Where-Object { $_.Name -like 'POSH_*' -or $_.Name -like 'STARSHIP_*' } |
            ForEach-Object { Remove-Item -LiteralPath "env:$($_.Name)" }
        foreach ($variable in $global:__compi_prompt_variables) {
            Set-Item -LiteralPath "env:$($variable.Name)" -Value $variable.Value
        }
        $function:global:prompt = $global:__compi_prompt_original
        __compi_prompt_load 'live.ps1' *> $null
        $global:__compi_prompt = $function:global:prompt
        $function:global:prompt = $wrapper
        try { [Microsoft.PowerShell.PSConsoleReadLine]::InvokePrompt() } catch { }
    }
}
# The folder as OSC 7, for a folder on a drive; Compi keeps the last one otherwise.
function global:__compi_location_report {
    $location = $ExecutionContext.SessionState.Path.CurrentLocation
    if ($location.Provider.Name -eq 'FileSystem' -and $location.ProviderPath -match '^[A-Za-z]:') {
        $path = $location.ProviderPath
        $segments = $path.Substring(2).Split('\') | ForEach-Object { [Uri]::EscapeDataString($_) }
        return "$([char]27)]7;file://localhost/$($path.Substring(0, 2))$($segments -join '/')$([char]7)"
    }
    ''
}
# The default WSL distribution and its user's home, read from the registry and the
# distribution's /etc/passwd until found once; $null without WSL.
function global:__compi_wsl {
    if (-not $global:__compi_wsl_found) {
        try {
            $lxss = 'HKCU:\Software\Microsoft\Windows\CurrentVersion\Lxss'
            $id = (Get-ItemProperty -LiteralPath $lxss -ErrorAction Stop).DefaultDistribution
            $entry = Get-ItemProperty -LiteralPath (Join-Path $lxss $id) -ErrorAction Stop
            $share = "\\wsl.localhost\$($entry.DistributionName)"
            if (-not (Test-Path -LiteralPath $share)) { $share = "\\wsl$\$($entry.DistributionName)" }
            $uid = "$($entry.DefaultUid)"
            $user = Get-Content -LiteralPath "$share\etc\passwd" -ErrorAction Stop |
                Where-Object { ($_ -split ':')[2] -eq $uid } | Select-Object -First 1
            $global:__compi_wsl_found = @{
                Name = $entry.DistributionName; Share = $share; Home = ($user -split ':')[5]
            }
        } catch { }
    }
    $global:__compi_wsl_found
}
# The Windows folder of a Linux path: wslpath follows Linux links, so ~/proj linked to
# /mnt/c/... is that C:\ folder; else the path through WSL's share.
function global:__compi_linux_folder([string] $linux) {
    $wsl = __compi_wsl
    if (-not $wsl) { return $null }
    $encoding = [Console]::OutputEncoding
    $exitCode = $global:LASTEXITCODE
    try {
        [Console]::OutputEncoding = [Text.Encoding]::UTF8
        $folder = & wsl.exe --distribution $wsl.Name --exec wslpath -a -w -- $linux 2> $null
        if ($LASTEXITCODE -ne 0) { $folder = $null }
    } catch { $folder = $null } finally {
        [Console]::OutputEncoding = $encoding
        $global:LASTEXITCODE = $exitCode
    }
    if ($folder) { return "$folder".Trim() }
    $wsl.Share + $linux.Replace('/', '\')
}
# Where a path into a Windows folder OneDrive (or a policy) moved really is: ~/Desktop,
# Desktop typed at home, or C:\Users\me\Desktop can be C:\Users\me\OneDrive\Desktop. The
# same for Documents, Pictures, Music and Videos; $null when that is not the case.
function global:__compi_moved_folder([string] $target) {
    $root = $HOME.TrimEnd('\')
    $path = $target.Replace('/', '\')
    if ($path -eq '~' -or $path.StartsWith('~\')) {
        $path = $root + $path.Substring(1)
    } elseif (-not [IO.Path]::IsPathRooted($path) -and
              (Get-Location).ProviderPath.TrimEnd('\') -eq $root) {
        $path = "$root\$path"
    }
    if (-not $path.StartsWith("$root\", [StringComparison]::OrdinalIgnoreCase)) { return $null }
    $relative = $path.Substring($root.Length + 1)
    $name = $relative.Split('\')[0]
    $special = @{ Desktop = 'Desktop'; Documents = 'MyDocuments'; Pictures = 'MyPictures'
                  Music = 'MyMusic'; Videos = 'MyVideos' }[$name]
    if (-not $special) { return $null }
    $folder = [Environment]::GetFolderPath($special) + $relative.Substring($name.Length)
    if ($folder -ne $path -and (Test-Path -LiteralPath $folder -PathType Container)) { $folder }
}
# Where a path PowerShell cannot find is on the other side: a moved Windows folder (see
# above); a WSL drive mount (/mnt/c/x) or a reported drive folder (/C:/x) on that drive;
# a Linux path (/x, ~/x) in WSL; and a relative path typed at your Windows home in your
# Linux home.
function global:__compi_other_side([string] $target) {
    $moved = __compi_moved_folder $target
    if ($moved) { return $moved }
    if ($target -match '^/mnt/([A-Za-z])(/.*)?$' -or $target -match '^/([A-Za-z]):(/.*)?$') {
        $drive = "$($Matches[1]):\" + "$($Matches[2])".TrimStart('/').Replace('/', '\')
        $moved = __compi_moved_folder $drive
        if ($moved) { return $moved }
        return $drive
    }
    $wsl = __compi_wsl
    if (-not $wsl) { return $null }
    if ($target -eq '~' -or $target.StartsWith('~/')) {
        $linux = $wsl.Home + $target.Substring(1)
    } elseif ($target.StartsWith('/')) {
        $linux = $target
    } elseif (-not [IO.Path]::IsPathRooted($target) -and
              (Get-Location).ProviderPath.TrimEnd('\') -eq $HOME.TrimEnd('\')) {
        $linux = "$($wsl.Home)/$target"
    } else {
        return $null
    }
    __compi_linux_folder $linux
}
# cd also takes a path from WSL's side; only one PowerShell cannot find changes, except
# a moved Windows folder, which wins over the empty one OneDrive leaves at the old place.
function global:__compi_cd {
    if ($args.Count -eq 1 -and $args[0] -is [string] -and -not $args[0].StartsWith('-')) {
        $other = __compi_moved_folder $args[0]
        if (-not $other -and -not (Test-Path -Path $args[0] -PathType Container)) {
            $other = __compi_other_side $args[0]
        }
        if ($other -and (Test-Path -LiteralPath $other -PathType Container)) {
            Set-Location -LiteralPath $other
            return
        }
    }
    Set-Location @args
}
# A cd your profile already changed (zoxide, say) stays yours.
if ((Get-Alias cd -ErrorAction SilentlyContinue).Definition -eq 'Set-Location') {
    Set-Alias -Name cd -Value __compi_cd -Option AllScope -Scope Global -Force
}
# compi z (or jump) and compi tree: Compi's picker answers with a folder, read without
# echo; other commands run the CLI.
if ($env:COMPI_CLI) {
    function global:compi {
        if ($args.Count -eq 1 -and $args[0] -in @('z', 'jump', 'tree')) {
            $action = if ($args[0] -eq 'tree') { 'tree' } else { 'jump' }
            [Console]::Write((__compi_location_report) + "$([char]27)]777;compi;$action$([char]7)")
            $encoded = ''
            while ($true) {
                $key = [Console]::ReadKey($true)
                if ($key.KeyChar -eq "`r" -or $key.KeyChar -eq "`n") { break }
                $encoded += $key.KeyChar
            }
            if (-not $encoded) { return }
            try {
                $folder = [Text.Encoding]::UTF8.GetString([Convert]::FromBase64String($encoded))
            } catch {
                Write-Error 'compi: invalid folder selection'
                return
            }
            if (Test-Path -LiteralPath $folder -PathType Container) {
                Set-Location -LiteralPath $folder
            } else {
                $other = __compi_other_side $folder
                if ($other -and (Test-Path -LiteralPath $other -PathType Container)) {
                    Set-Location -LiteralPath $other
                } else {
                    Write-Error "compi: $folder is not a folder here"
                }
            }
            return
        }
        if ($MyInvocation.ExpectingInput) { $input | & $env:COMPI_CLI @args } else { & $env:COMPI_CLI @args }
    }
}
function global:prompt {
    $succeeded = $?
    $exitCode = $global:LASTEXITCODE
    $esc = [char]27
    $bel = [char]7
    $report = ''
    if (-not $global:__compi_pid_reported) {
        $report += "$esc]777;compi;pid;$PID$bel"
        $global:__compi_pid_reported = $true
    }
    $report += __compi_location_report
    $report += "$esc]133;A;aid=$PID$bel"
    [Console]::Write($report)
    $global:LASTEXITCODE = $exitCode
    if (-not $succeeded) { Write-Error '' -ErrorAction Ignore }
    & $global:__compi_prompt
}
