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
    $location = $ExecutionContext.SessionState.Path.CurrentLocation
    if ($location.Provider.Name -eq 'FileSystem' -and $location.ProviderPath -match '^[A-Za-z]:') {
        $path = $location.ProviderPath
        $segments = $path.Substring(2).Split('\') | ForEach-Object { [Uri]::EscapeDataString($_) }
        $report += "$esc]7;file://localhost/$($path.Substring(0, 2))$($segments -join '/')$bel"
    }
    $report += "$esc]133;A;aid=$PID$bel"
    [Console]::Write($report)
    $global:LASTEXITCODE = $exitCode
    if (-not $succeeded) { Write-Error '' -ErrorAction Ignore }
    & $global:__compi_prompt
}
