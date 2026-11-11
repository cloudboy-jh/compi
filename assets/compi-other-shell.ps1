# Bash commands entered in a Windows PowerShell tab. The line editor displays and
# remembers the original line; only the string handed back to PowerShell changes.
# One WSL Bash per tab keeps variables and the working folder between commands.
function global:__compi_shell_argument([string] $text) {
    if ($text -notmatch '[\s"]') { return $text }
    '"' + [regex]::Replace([regex]::Replace($text, '(\\*)"', '$1$1\"'), '(\\+)$', '$1$1') + '"'
}

function global:__compi_bash_start {
    if ($global:__compi_bash -and -not $global:__compi_bash.HasExited) { return }
    $wsl = __compi_wsl
    if (-not $wsl) { throw 'No WSL distribution is installed.' }
    $global:__compi_bash_mark = 'compi' + [Guid]::NewGuid().ToString('N')
    $worker = @'
mark=$1
rm -f -- "$0"
trap ':' INT
printf '\036%sP%s\037' "$mark" "$$"
while IFS= read -r kind && IFS= read -r folder && IFS= read -r encoded; do
    text=$(printf '%s' "$encoded" | base64 -d)
    folder=$(printf '%s' "$folder" | base64 -d)
    if [ -n "$folder" ]; then
        folder=$(wslpath -u -- "$folder") && builtin cd -- "$folder"
    fi
    if [ "$kind" = find ]; then
        command -v -- "$text" > /dev/null 2>&1
        code=$?
    else
        before=$PWD
        eval -- "$text"
        code=$?
    fi
    printf '\036%sO%s;%s\037' "$mark" "$code" "$PWD"
    printf '\036%sE\037' "$mark" >&2
done
'@
    $info = New-Object Diagnostics.ProcessStartInfo
    $info.FileName = 'wsl.exe'
    $workerPath = '/tmp/' + $global:__compi_bash_mark + '.sh'
    $nativeWorker = $wsl.Share + $workerPath.Replace('/', '\')
    $utf8 = New-Object Text.UTF8Encoding $false
    $file = [IO.File]::Open($nativeWorker, [IO.FileMode]::CreateNew)
    try {
        $bytes = $utf8.GetBytes($worker)
        $file.Write($bytes, 0, $bytes.Length)
    } finally { $file.Dispose() }
    $info.Arguments = '--distribution ' + (__compi_shell_argument $wsl.Name) +
        ' --exec setsid --wait /bin/bash -l ' + $workerPath + ' ' + $global:__compi_bash_mark
    $info.UseShellExecute = $false
    $info.CreateNoWindow = $true
    $info.RedirectStandardInput = $true
    $info.RedirectStandardOutput = $true
    $info.RedirectStandardError = $true
    $utf8 = New-Object Text.UTF8Encoding $false
    $info.StandardOutputEncoding = $utf8
    $info.StandardErrorEncoding = $utf8
    $global:__compi_bash = [Diagnostics.Process]::Start($info)
    $global:__compi_bash.StandardInput.NewLine = "`n"
    $global:__compi_bash_folder = ''
    $global:__compi_bash_variables = @{}
    $global:__compi_bash_buffers = @('', '')
    $global:__compi_bash_chars = @((New-Object char[] 1024), (New-Object char[] 1024))
    $global:__compi_bash_reads = @(
        $global:__compi_bash.StandardOutput.ReadAsync($global:__compi_bash_chars[0], 0, 1024),
        $global:__compi_bash.StandardError.ReadAsync($global:__compi_bash_chars[1], 0, 1024)
    )
}

# Streams both pipes without waiting for a line break. Only this tab's random marker
# is consumed. Keyboard input belongs to Bash while a command runs.
function global:__compi_bash_exchange([string] $kind, [string] $text) {
    __compi_bash_start
    $place = (Get-Location).ProviderPath
    $folder = if ($place -ne $global:__compi_bash_folder) { $place } else { '' }
    $encode = { param($value) [Convert]::ToBase64String([Text.Encoding]::UTF8.GetBytes($value)) }
    $global:__compi_bash.StandardInput.WriteLine($kind)
    $global:__compi_bash.StandardInput.WriteLine((& $encode $folder))
    $global:__compi_bash.StandardInput.WriteLine((& $encode $text))
    $global:__compi_bash.StandardInput.Flush()
    $seen = 0
    $code = 1
    $keys = -not [Console]::IsInputRedirected
    $oldControl = $false
    if ($keys) { $oldControl = [Console]::TreatControlCAsInput; [Console]::TreatControlCAsInput = $true }
    try {
        while ($seen -ne 3) {
            for ($stream = 0; $stream -lt 2; $stream++) {
                $read = $global:__compi_bash_reads[$stream]
                if (-not $read.IsCompleted) { continue }
                $n = $read.GetAwaiter().GetResult()
                if ($n -eq 0) { throw 'The Bash companion stopped.' }
                $buffer = $global:__compi_bash_buffers[$stream] + [string]::new($global:__compi_bash_chars[$stream], 0, $n)
                while ($buffer.Length) {
                    $start = $buffer.IndexOf([char]30)
                    if ($start -lt 0) {
                        if ($kind -eq 'run') { [Console]::Write($buffer) }
                        $buffer = ''; break
                    }
                    if ($start -gt 0 -and $kind -eq 'run') { [Console]::Write($buffer.Substring(0, $start)) }
                    $buffer = $buffer.Substring($start)
                    $end = $buffer.IndexOf([char]31)
                    if ($end -lt 0) { break }
                    $record = $buffer.Substring(1, $end - 1)
                    $buffer = $buffer.Substring($end + 1)
                    $mark = $global:__compi_bash_mark
                    if ($record.StartsWith($mark + 'P')) {
                        $global:__compi_bash_pid = $record.Substring($mark.Length + 1)
                    } elseif ($record.StartsWith($mark + 'O')) {
                        $parts = $record.Substring($mark.Length + 1) -split ';', 2
                        $code = [int]$parts[0]
                        if ($kind -eq 'run' -and $parts.Count -gt 1) {
                            $native = __compi_other_side $parts[1]
                            if ($native -and (Test-Path -LiteralPath $native -PathType Container)) {
                                Set-Location -LiteralPath $native
                            }
                            $global:__compi_bash_folder = (Get-Location).ProviderPath
                        }
                        $seen = $seen -bor 1
                    } elseif ($record -eq $mark + 'E') {
                        $seen = $seen -bor 2
                    } elseif ($kind -eq 'run') {
                        [Console]::Write("$([char]30)$record$([char]31)")
                    }
                }
                $global:__compi_bash_buffers[$stream] = $buffer
                $reader = if ($stream -eq 0) { $global:__compi_bash.StandardOutput } else { $global:__compi_bash.StandardError }
                $global:__compi_bash_reads[$stream] = $reader.ReadAsync($global:__compi_bash_chars[$stream], 0, 1024)
            }
            if ($seen -ne 3 -and $kind -eq 'run' -and $keys -and [Console]::KeyAvailable) {
                $key = [Console]::ReadKey($true)
                if ([int]$key.KeyChar -eq 3) {
                    [Console]::WriteLine('^C')
                    $wsl = __compi_wsl
                    & wsl.exe --distribution $wsl.Name --exec /bin/kill -INT -- "-$global:__compi_bash_pid" *> $null
                } else {
                    $char = if ($key.KeyChar -eq "`r") { "`n" } else { [string]$key.KeyChar }
                    [Console]::Write($char)
                    $global:__compi_bash.StandardInput.Write($char)
                    $global:__compi_bash.StandardInput.Flush()
                }
            }
            if ($seen -ne 3) { [Threading.Thread]::Sleep(10) }
        }
    } finally {
        if ($keys) { [Console]::TreatControlCAsInput = $oldControl }
    }
    $code
}

function global:__compi_bash_line([string] $line) {
    $text = $line.TrimStart()
    if (-not $text) { return $false }
    # An exported Bash variable remains Bash's unless PowerShell has one too.
    foreach ($match in [regex]::Matches($text, '\$(?:\{)?([A-Za-z_][A-Za-z0-9_]*)')) {
        $name = $match.Groups[1].Value
        if ($global:__compi_bash_variables -and $global:__compi_bash_variables.ContainsKey($name) -and
            -not (Get-Variable -Name $name -ErrorAction SilentlyContinue)) { return $true }
    }
    $first = ($text -split '\s+', 2)[0]
    if ($text -match '^[A-Za-z_][A-Za-z0-9_]*=' -or
        $text -match '^\[\[\s' -or $text -match '^(if|for|while|until)\s.*[;\r\n]\s*(then|do)\b') { return $true }
    if (Get-Command -Name $first -ErrorAction SilentlyContinue) { return $false }
    if ($first -eq 'export' -or $first -eq 'source') { return $true }
    # Punctuation in an unknown word may be PowerShell syntax; do not guess.
    if ($first -notmatch '^[A-Za-z_][A-Za-z0-9_.+-]*$') { return $false }
    (__compi_bash_exchange 'find' $first) -eq 0
}

function global:__compi_bash_run {
    $line = $global:__compi_bash_line_text
    $global:__compi_bash_line_text = $null
    try {
        $code = __compi_bash_exchange 'run' $line
        foreach ($match in [regex]::Matches($line, '(?:^|[;\r\n])\s*(?:export\s+)?([A-Za-z_][A-Za-z0-9_]*)=')) {
            $global:__compi_bash_variables[$match.Groups[1].Value] = $true
        }
        $global:LASTEXITCODE = $code
        if ($code -ne 0) { Write-Error '' -ErrorAction Ignore }
    } catch {
        $global:LASTEXITCODE = 1
        Write-Error $_
    }
}

$global:__compi_other_enabled = $env:COMPI_OTHER_SHELL_COMMANDS -ne '0' -and [bool](__compi_wsl)
if ($global:__compi_other_enabled) {
    Import-Module PSReadLine -ErrorAction Continue
}
if ($global:__compi_other_enabled -and (Get-Command PSConsoleHostReadLine -ErrorAction SilentlyContinue)) {
    $global:__compi_reader_status_parameter = [bool]([Microsoft.PowerShell.PSConsoleReadLine].GetMethods() |
        Where-Object { $_.Name -eq 'ReadLine' -and $_.GetParameters().Count -eq 3 -and $_.GetParameters()[2].ParameterType.FullName -like '*Boolean*' })
    function global:PSConsoleHostReadLine {
        $ok = $?
        Microsoft.PowerShell.Core\Set-StrictMode -Off
        if ($global:__compi_reader_status_parameter) {
            $line = [Microsoft.PowerShell.PSConsoleReadLine]::ReadLine($host.Runspace, $ExecutionContext, $ok)
        } else {
            $line = [Microsoft.PowerShell.PSConsoleReadLine]::ReadLine($host.Runspace, $ExecutionContext)
        }
        if (__compi_bash_line $line) {
            $global:__compi_bash_line_text = $line
            return '__compi_bash_run'
        }
        $line
    }
}
