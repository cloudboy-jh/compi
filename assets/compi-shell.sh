# Source in interactive Bash or Zsh: . /path/to/compi/assets/compi-shell.sh
# Only tree/z/jump are intercepted; other compi commands retain their normal CLI behavior.
# Inside WSL, cd also takes a Windows path.

# Initialize after user startup files, including login Bash's imported functions.
_compi_cli_init() {
    [[ -z ${_compi_cli_ready-} ]] || return 0
    if [[ -n ${COMPI_CLI_WINDOWS-} ]]; then
        COMPI_CLI=$(command wslpath -u "$COMPI_CLI_WINDOWS") || return 1
        if [[ -n ${COMPI_CLI_DATA_DIR_WINDOWS-} ]]; then
            COMPI_DATA_DIR=$(command wslpath -u "$COMPI_CLI_DATA_DIR_WINDOWS") || return 1
            export COMPI_DATA_DIR
        fi
        export WSLENV="${WSLENV:+$WSLENV:}COMPI_INSTANCE:COMPI_SURFACE_ID:WSL_DISTRO_NAME:COMPI_DATA_DIR/p:COMPI_SHELL_CWD"
    fi
    [[ -n ${COMPI_CLI-} ]] || return 0
    export COMPI_CLI
    case ":$PATH:" in
        *":$HOME/.compi/shell:"*) ;;
        *) export PATH="$HOME/.compi/shell:$PATH" ;;
    esac
    _compi_cli_ready=1
}

_compi_report_cwd() {
    local LC_ALL=C uri='' char escaped index
    for ((index = 0; index < ${#PWD}; index++)); do
        if [[ -n ${ZSH_VERSION-} ]]; then
            char=${PWD[index+1]}
        else
            char=${PWD:index:1}
        fi
        case "$char" in
            [a-zA-Z0-9/._~-]) uri+=$char ;;
            *)
                printf -v escaped '%%%02X' "'$char"
                uri+=$escaped
                ;;
        esac
    done
    printf '\033]7;file://localhost%s\a' "$uri" > /dev/tty || return 1
    _compi_last_reported_cwd=$PWD
}

# Runs at every prompt. It keeps the exit status for prompt hooks that run
# after it, so a prompt's exit-status segment stays correct.
_compi_prompt_cwd() {
    local compi_status=$?
    _compi_cli_init
    if [[ -z ${_compi_pid_reported-} ]] && [[ -t 0 || -t 1 || -t 2 ]]; then
        printf '\033]777;compi;pid;%s\a' "$$" > /dev/tty && _compi_pid_reported=1
    fi
    # A new prompt (OSC 133 A) ends the work programs reported through OSC 7501. Its aid
    # names this shell, so programs that mark their own prompts end nothing.
    if [[ -t 0 || -t 1 || -t 2 ]]; then
        printf '\033]133;A;aid=%s\a' "$$" > /dev/tty
    fi
    if [[ -n ${ZSH_VERSION-} ]]; then
        _compi_prompt_sync "$compi_status"
    elif [[ -z ${BLE_VERSION-} && -z ${_compi_prompt_native_dispatching-} &&
            ${PROMPT_COMMAND-} != _compi_prompt_native_dispatch ]]; then
        # Imported login hooks have no rc wrapper; bootstrap their dispatcher.
        _compi_prompt_sync "$compi_status"
        _compi_install_prompt_hook
    fi
    if [[ -t 0 || -t 1 || -t 2 ]] && [[ ${_compi_last_reported_cwd-} != "$PWD" ]]; then
        _compi_report_cwd
    fi
    # Commands from the other shell (see _compi_other_setup) start at the first prompt
    # in WSL. Each prompt after that only marks a new command line for Enter.
    if [[ -n ${WSL_DISTRO_NAME-} && ${_compi_other_state-} != off ]]; then
        if [[ -z ${_compi_other_state-} ]]; then
            _compi_other_state=off
            if ! typeset -f _compi_other_setup > /dev/null &&
               [[ -r $HOME/.compi/shell/compi-shell.sh ]]; then
                _compi_other_loading=1
                . "$HOME/.compi/shell/compi-shell.sh"
                unset _compi_other_loading
            fi
            ! typeset -f _compi_other_setup > /dev/null || _compi_other_setup
        fi
        _compi_other_primary=1
        [[ -z ${_compi_other_stderr-} ]] || _compi_other_restore
    fi
    return "$compi_status"
}

# ble.sh calls ATTACH and PRECMD hooks with the user's PS1 and PROMPT_COMMAND.
_compi_prompt_ble_hook() {
    local compi_status=$? _compi_prompt_ble_context=1
    _compi_prompt_sync "$compi_status"
    return "$compi_status"
}

# PRECMD runs before ble.sh evaluates PROMPT_COMMAND, including after a reload.
_compi_prompt_ble_precmd() {
    local compi_status=$? _compi_prompt_ble_context=1
    _compi_prompt_sync "$compi_status" precmd
    return "$compi_status"
}

# Restore the user's global command value, including sparse array indices.
_compi_prompt_command_restore() {
    local declaration=${1-} attributes
    unset PROMPT_COMMAND
    [[ -n $declaration ]] || return 0
    if (( BASH_VERSINFO[0] > 4 ||
          (BASH_VERSINFO[0] == 4 && BASH_VERSINFO[1] >= 2) )); then
        eval "${declaration/#declare /declare -g }"
    else
        # Bash 3.2 has no declare -g; assignments inside functions stay global.
        attributes=${declaration%% PROMPT_COMMAND*}
        attributes=${attributes#declare -}
        [[ $attributes != *a* ]] || PROMPT_COMMAND=()
        case $declaration in
            *" PROMPT_COMMAND="*) eval "PROMPT_COMMAND${declaration#* PROMPT_COMMAND}" ;;
        esac
        [[ $attributes != *x* ]] || export PROMPT_COMMAND
    fi
}

# Native Bash parses scalar PROMPT_COMMAND before running any of it. Dispatch
# the selected hooks ourselves so reloads never run both old and new providers.
_compi_prompt_native_dispatch() {
    local compi_status=$? compi_lastarg=$_ compi_pipeline=( "${PIPESTATUS[@]}" )
    local _compi_prompt_native_dispatching=1 hook compi_context='' compi_pipe
    # Bash restores the original context for every array entry. A real status
    # pipeline is needed only for multi-command PIPESTATUS, never normal prompts.
    if (( ${#compi_pipeline[@]} > 1 )); then
        for compi_pipe in "${compi_pipeline[@]}"; do
            compi_context+=" | (builtin exit $compi_pipe)"
        done
        compi_context=${compi_context:3}
    fi
    _compi_prompt_command_restore "${_compi_native_prompt_command-}"
    _compi_prompt_sync "$compi_status" native
    for hook in "${PROMPT_COMMAND[@]}"; do
        _compi_prompt_status "$compi_status" "$compi_lastarg"
        eval "$compi_context"$'\n'"$hook"
    done
    _compi_prompt_status "$compi_status"
    _compi_prompt_cwd
    _compi_prompt_native_dispatching=
    _compi_install_prompt_hook
}

# Prompt settings write ~/.compi/prompt/compi.{bash,zsh} for Compi shells. When
# a change is applied to running shells they also write live.{bash,zsh} and a
# new reload token; each Compi shell reloads at its next prompt. Compi never
# types into a terminal to change a prompt.
_compi_prompt_sync() {
    [[ -z ${_compi_prompt_rerunning-} ]] || return 0
    local shell=bash token=''
    [[ -z ${ZSH_VERSION-} ]] || shell=zsh
    if [[ -z ${_compi_prompt_loaded-} ]]; then
        # Login Bash has no Compi startup wrapper, so it loads at its first prompt.
        if _compi_prompt_startup; then
            . "$HOME/.compi/prompt/compi.$shell"
            _compi_install_prompt_hook
            [[ ${2-} == precmd || ${2-} == native ]] || _compi_prompt_rerun "$1"
        fi
        return 0
    fi
    [[ ! -f $HOME/.compi/prompt/reload ]] || IFS= read -r token < "$HOME/.compi/prompt/reload"
    [[ $token != "${_compi_prompt_token-}" ]] || return 0
    _compi_prompt_token=$token
    _compi_prompt_restore
    [[ ! -f $HOME/.compi/prompt/live.$shell ]] || . "$HOME/.compi/prompt/live.$shell"
    _compi_install_prompt_hook
    [[ ${2-} == precmd || ${2-} == native ]] || _compi_prompt_rerun "$1"
}

# Startup wrappers call this after the user's rc and, when it succeeds, source
# ~/.compi/prompt/compi.<shell> at top level so its declarations stay global.
# ble.sh takes over PS1 and PROMPT_COMMAND when it attaches, so while it is
# loaded but not attached yet the prompt loads from its ATTACH hook instead.
# An rc that ends with `ble-attach` normally loads it in _compi_ble_attach; if
# ble.sh attached some other way, load now with the user's variables handed
# back, and _compi_enable_prompt_cwd returns them to ble.sh and redraws.
_compi_prompt_startup() {
    local shell=bash token=''
    [[ -z ${_compi_prompt_loaded-} ]] || return 1
    if [[ -n ${BLE_VERSION-} && -z ${_compi_prompt_ble_context-} ]]; then
        [[ -n ${_ble_attached-} ]] && ble-edit/restore-PS1 || return 1
        _compi_prompt_ble_restored=1
    fi
    [[ -z ${ZSH_VERSION-} ]] || shell=zsh
    _compi_prompt_loaded=1
    [[ ! -f $HOME/.compi/prompt/reload ]] || IFS= read -r token < "$HOME/.compi/prompt/reload"
    _compi_prompt_token=$token
    _compi_prompt_snapshot
    [[ -f $HOME/.compi/prompt/compi.$shell ]]
}

# ble.sh draws the first prompt inside `ble-attach`, before its ATTACH hooks.
# Startup wrappers alias `ble-attach` to this while the user's rc runs, so an rc
# ending with `ble-attach` attaches with the applied prompt already loaded and
# never shows the plain prompt first. The alias removes itself on first use.
_compi_ble_attach() {
    [[ ${BASH_ALIASES[ble-attach]-} != _compi_ble_attach ]] || unalias ble-attach
    if [[ -z ${_ble_attached-} ]]; then
        local _compi_prompt_ble_context=1
        if [[ ${PROMPT_COMMAND-} == _compi_prompt_native_dispatch ]]; then
            _compi_prompt_command_restore "${_compi_native_prompt_command-}"
            unset _compi_native_prompt_command _compi_native_prompt_attributes
            unset _compi_native_prompt_values _compi_native_prompt_indices
        fi
        if _compi_prompt_startup; then
            . "$HOME/.compi/prompt/compi.bash"
        fi
        _compi_install_prompt_hook
    fi
    ble-attach "$@"
}

# Remember the prompt as the user's startup files left it, before any prompt
# Compi manages, so a later reload or turn-off can return to it. Oh My Posh and
# Starship keep their session in POSH_*, _omp_* and STARSHIP_* variables, and
# under ble.sh in its prompt options and hooks.
_compi_prompt_snapshot() {
    [[ -z ${_compi_prompt_saved-} ]] || return 0
    _compi_prompt_saved=1
    if [[ -n ${ZSH_VERSION-} ]]; then
        _compi_saved_ps1=$PS1 _compi_saved_rps1=${RPS1-} _compi_saved_ps2=$PS2
        _compi_saved_precmd=( "${precmd_functions[@]}" )
        _compi_saved_preexec=( "${preexec_functions[@]}" )
        _compi_saved_provider=$(typeset -p -m 'POSH_*' '_omp_*' 'STARSHIP_*' 2>/dev/null)
    else
        _compi_saved_ps0=${PS0-} _compi_saved_ps1=${PS1-} _compi_saved_ps2=${PS2-}
        _compi_saved_prompt_command=$(declare -p PROMPT_COMMAND 2>/dev/null)
        _compi_saved_debug=$(trap -p DEBUG)
        _compi_saved_provider=$(
            names=$(compgen -v POSH_; compgen -v _omp_; compgen -v STARSHIP_)
            [[ -z $names ]] || declare -p $names 2>/dev/null
        )
        if [[ -n ${BLE_VERSION-} ]]; then
            _compi_saved_ble=$(
                bleopt prompt_rps1 prompt_ps1_final prompt_ps1_transient 2>/dev/null
                blehook PRECMD PREEXEC 2>/dev/null
            )
        fi
    fi
}

_compi_prompt_restore() {
    [[ -n ${_compi_prompt_saved-} ]] || return 0
    if [[ -n ${ZSH_VERSION-} ]]; then
        PS1=$_compi_saved_ps1 RPS1=$_compi_saved_rps1 PS2=$_compi_saved_ps2
        precmd_functions=( "${_compi_saved_precmd[@]}" )
        preexec_functions=( "${_compi_saved_preexec[@]}" )
        unset -m 'POSH_*' '_omp_*' 'STARSHIP_*' 2>/dev/null
        eval "$_compi_saved_provider"
    else
        PS0=$_compi_saved_ps0 PS1=$_compi_saved_ps1 PS2=$_compi_saved_ps2
        _compi_prompt_command_restore "${_compi_saved_prompt_command-}"
        if [[ $(trap -p DEBUG) != "$_compi_saved_debug" ]]; then
            if [[ -n $_compi_saved_debug ]]; then eval "$_compi_saved_debug"; else trap - DEBUG; fi
        fi
        local name saved=$'\n'$_compi_saved_provider
        for name in $(compgen -v POSH_; compgen -v _omp_; compgen -v STARSHIP_); do
            unset -v "$name" 2>/dev/null
        done
        eval "${saved//$'\n'declare /$'\n'declare -g }"
        if [[ -n ${BLE_VERSION-} ]]; then
            blehook PRECMD=
            blehook PREEXEC=
            eval "${_compi_saved_ble-}"
        fi
    fi
}

_compi_prompt_status() {
    return "$1"
}

# Run the new prompt hooks once so this prompt already uses the reloaded style.
_compi_prompt_rerun() {
    local _compi_prompt_rerunning=1 hook
    if [[ -n ${ZSH_VERSION-} ]]; then
        if (( ${+functions[precmd]} )); then _compi_prompt_status "$1"; precmd; fi
        for hook in "${precmd_functions[@]}"; do
            [[ $hook == _compi_prompt_cwd ]] && continue
            _compi_prompt_status "$1"
            "$hook"
        done
    else
        for hook in "${PROMPT_COMMAND[@]}"; do
            _compi_prompt_status "$1"
            eval "$hook"
        done
    fi
    return 0
}

# ble.sh keeps the cwd hook last and preserves array indices it tracks. Native
# Bash uses a dispatcher that selects the provider before executing user hooks.
_compi_install_prompt_hook() {
    if [[ -n ${ZSH_VERSION-} ]]; then
        precmd_functions=( ${precmd_functions:#_compi_prompt_cwd} _compi_prompt_cwd )
        return 0
    fi
    if [[ -z ${BLE_VERSION-} ]]; then
        [[ -z ${_compi_prompt_native_dispatching-} ]] || return 0
        [[ ${PROMPT_COMMAND-} != _compi_prompt_native_dispatch ]] || return 0
        local index position=0 unchanged='' attributes=''
        if (( BASH_VERSINFO[0] > 4 ||
              (BASH_VERSINFO[0] == 4 && BASH_VERSINFO[1] >= 4) )); then
            attributes=${PROMPT_COMMAND@a}
            if [[ $attributes == "${_compi_native_prompt_attributes-}" &&
                  ${#PROMPT_COMMAND[@]} == ${#_compi_native_prompt_values[@]} ]]; then
                unchanged=1
                for index in "${!PROMPT_COMMAND[@]}"; do
                    if [[ $index != "${_compi_native_prompt_indices[position]}" ||
                          ${PROMPT_COMMAND[index]} != "${_compi_native_prompt_values[position]}" ]]; then
                        unchanged=''
                        break
                    fi
                    ((position += 1))
                done
            fi
        fi
        if [[ -z $unchanged ]]; then
            _compi_native_prompt_command=$(declare -p PROMPT_COMMAND 2>/dev/null)
            _compi_native_prompt_attributes=$attributes
            _compi_native_prompt_values=( "${PROMPT_COMMAND[@]}" )
            _compi_native_prompt_indices=( "${!PROMPT_COMMAND[@]}" )
        fi
        unset PROMPT_COMMAND
        PROMPT_COMMAND=_compi_prompt_native_dispatch
        return 0
    fi
    case "$(declare -p PROMPT_COMMAND 2>/dev/null)" in
        "declare -a"*)
            local index last=''
            for index in "${!PROMPT_COMMAND[@]}"; do last=$index; done
            if [[ -z $last || ${PROMPT_COMMAND[last]} != _compi_prompt_cwd ]]; then
                for index in "${!PROMPT_COMMAND[@]}"; do
                    [[ ${PROMPT_COMMAND[index]} != _compi_prompt_cwd ]] || unset 'PROMPT_COMMAND[index]'
                done
                PROMPT_COMMAND+=( _compi_prompt_cwd )
            fi
            ;;
        *)
            case "${PROMPT_COMMAND-}" in
                *$'\n'_compi_prompt_cwd|*'; _compi_prompt_cwd'|_compi_prompt_cwd) ;;
                '') PROMPT_COMMAND=_compi_prompt_cwd ;;
                *) PROMPT_COMMAND="${PROMPT_COMMAND}"$'\n''_compi_prompt_cwd' ;;
            esac
            ;;
    esac
    if [[ -n ${BLE_VERSION-} ]]; then
        blehook ATTACH!=_compi_prompt_ble_hook PRECMD!=_compi_prompt_ble_precmd
    fi
}

# Called after the user's shell rc, so existing prompt hooks are preserved.
_compi_enable_prompt_cwd() {
    case $- in *i*) ;; *) return 0 ;; esac
    [[ -z ${BASH_VERSION-} || -z ${BLE_VERSION-} ||
       ${BASH_ALIASES[ble-attach]-} != _compi_ble_attach ]] ||
        unalias ble-attach
    _compi_install_prompt_hook
    _compi_prompt_cwd
    if [[ -n ${_compi_prompt_ble_restored-} ]]; then
        # ble.sh drew the first prompt while attaching; draw it again with the
        # loaded style, the way ble-attach does after its ATTACH hooks.
        unset -v _compi_prompt_ble_restored
        [[ ! -f $HOME/.compi/prompt/compi.bash ]] || _compi_prompt_rerun 0
        ble-edit/adjust-PS1
        ble/prompt/clear
        ble/textarea#redraw
    fi
}

_compi_choose_directory() {
    local LC_ALL=C action=$1 encoded directory='' alphabet
    local prefix char octal byte value=0 bits=0 digits=0 padding=0 index
    alphabet='ABCDEFGHIJKLMNOPQRSTUVWXYZabcdefghijklmnopqrstuvwxyz0123456789+/'
    # read -s silences the terminal only during this read, and restores echo on
    # completion or interruption; no permanent stty changes or prompt input.
    _compi_report_cwd || return 1
    printf '\033]777;compi;%s\a' "$action" > /dev/tty || return 1
    IFS= read -r -s encoded < /dev/tty || return 1
    [[ -n $encoded ]] || return 0  # Empty reply means the picker was canceled.

    # Decode canonical base64 with shell builtins: a directory jump must not
    # launch base64, tr, uname or a subshell in the interactive terminal.
    if (( ${#encoded} % 4 )); then
        printf 'compi: invalid directory selection\n' > /dev/tty
        return 1
    fi
    for ((index = 0; index < ${#encoded}; index++)); do
        if [[ -n ${ZSH_VERSION-} ]]; then
            char=${encoded[index+1]}
        else
            char=${encoded:index:1}
        fi
        if [[ $char == = ]]; then
            ((padding++))
            continue
        fi
        prefix=${alphabet%%"$char"*}
        if (( padding || ${#prefix} == 64 )); then
            printf 'compi: invalid directory selection\n' > /dev/tty
            return 1
        fi
        ((digits++))
        value=$(( (value << 6) | ${#prefix} ))
        bits=$(( bits + 6 ))
        if (( bits >= 8 )); then
            bits=$(( bits - 8 ))
            byte=$(( (value >> bits) & 255 ))
            if (( byte == 0 )); then
                printf 'compi: invalid directory selection\n' > /dev/tty
                return 1
            fi
            printf -v octal '\\0%03o' "$byte"
            printf -v char '%b' "$octal"
            directory+=$char
            value=$(( value & ((1 << bits) - 1) ))
        fi
    done
    if (( padding > 2 || digits % 4 == 1 ||
          (digits % 4 == 2 && padding != 2) ||
          (digits % 4 == 3 && padding != 1) ||
          (digits % 4 == 0 && padding != 0) || value != 0 )); then
        printf 'compi: invalid directory selection\n' > /dev/tty
        return 1
    fi
    # A folder a PowerShell pane visited is a Windows path.
    if [[ ! -d $directory ]]; then
        encoded=$(_compi_cd_target "$directory") && [[ -d $encoded ]] && directory=$encoded
    fi
    if [[ $directory != /* || ! -d $directory ]]; then
        printf 'compi: invalid directory selection\n' > /dev/tty
        return 1
    fi
    builtin cd -- "$directory" || return 1
    _compi_report_cwd
}

# The Windows folders OneDrive (or a policy) can move out of the Windows home, as Compi
# passes them to the WSL shells it starts, `|`-separated: Home=C:\Users\me, then
# Desktop=C:\Users\me\OneDrive\Desktop and the same for Documents, Pictures, Music, Videos.
_compi_windows_folder() {
    local list=${COMPI_WINDOWS_FOLDERS-} entry
    while [[ -n $list ]]; do
        entry=${list%%|*}
        if [[ $list == *'|'* ]]; then list=${list#*|}; else list=''; fi
        if [[ ${entry%%=*} == "$1" ]]; then
            printf '%s\n' "${entry#*=}"
            return 0
        fi
    done
    return 1
}

# Where a path into one of those folders really is: ~/Desktop/x, Desktop/x typed at
# home, or /mnt/c/Users/me/Desktop/x.
_compi_moved_folder() {
    local target=$1 relative name folder home
    if [[ $target == "$HOME"/* ]]; then
        relative=${target#"$HOME"/}
    elif [[ $target != /* && $PWD == "$HOME" ]]; then
        relative=$target
    elif home=$(_compi_windows_folder Home) && home=$(command wslpath -u "$home" 2> /dev/null) &&
         [[ $target == "$home"/* ]]; then
        relative=${target#"$home"/}
    else
        return 1
    fi
    name=${relative%%/*}
    [[ $name != Home ]] && folder=$(_compi_windows_folder "$name") &&
        folder=$(command wslpath -u "$folder" 2> /dev/null) || return 1
    folder+=${relative#"$name"}
    [[ $folder != "$target" && -d $folder ]] || return 1
    printf '%s\n' "$folder"
}

# Inside WSL, where a Windows path is: C:\x, C:/x, or the /C:/x a PowerShell pane
# reports, at the drive mount wslpath names; \\wsl.localhost\<this distribution>\x
# (or \\wsl$\…) at /x; and a moved Windows folder where it went. Fails for any other
# path, and outside WSL.
_compi_cd_target() {
    local target=$1 name
    [[ -n ${WSL_DISTRO_NAME-} ]] || return 1
    case $target in
        /[A-Za-z]:|/[A-Za-z]:/*) target=${target#/} ;;
    esac
    case $target in
        [A-Za-z]:) target=$(command wslpath -u "$target\\" 2> /dev/null) || return 1 ;;
        [A-Za-z]:[\\/]*) target=$(command wslpath -u "$target" 2> /dev/null) || return 1 ;;
        [\\/][\\/]wsl.localhost[\\/]*|[\\/][\\/]wsl\$[\\/]*)
            target=${target#??}
            target=${target#*[\\/]}
            name=${target%%[\\/]*}
            [[ $name == "$WSL_DISTRO_NAME" ]] || return 1
            target=${target#"$name"}
            target=${target//\\//}
            printf '%s\n' "${target:-/}"
            return 0
            ;;
    esac
    if [[ $target != "$1" ]]; then
        _compi_moved_folder "$target" && return 0
        [[ -d $target ]] || return 1
        printf '%s\n' "$target"
        return 0
    fi
    _compi_moved_folder "$target"
}

# Commands from the other shell. In a WSL tab, a line that is a PowerShell command runs
# in PowerShell when Enter is pressed: the line stays on screen as typed, its output
# follows, and history keeps it. One PowerShell per tab (pwsh.exe, else powershell.exe)
# starts on first use and keeps its variables and folder for later lines. Compi turns
# this off with COMPI_OTHER_SHELL_COMMANDS=0.

# Runs once, at the first prompt, after the startup files: Enter gets a check of the
# line. Bash before 4.4, ble.sh (its own line editor), a Zsh without a UTF-8 locale and
# a WSL that cannot start Windows programs keep Enter as it is.
_compi_other_setup() {
    _compi_other_state=off
    [[ ${COMPI_OTHER_SHELL_COMMANDS-} != 0 ]] || return 0
    if [[ -n ${ZSH_VERSION-} ]]; then
        local blank=$'\xc2\xa0'
        [[ -o zle && ${#blank} == 1 ]] && zmodload zsh/system 2> /dev/null || return 0
        if whence -p pwsh.exe > /dev/null; then
            _compi_other_exe=pwsh.exe
        elif whence -p powershell.exe > /dev/null; then
            _compi_other_exe=powershell.exe
        else
            return 0
        fi
        zle -A accept-line _compi_other_accepted
        zle -N accept-line _compi_other_accept
        functions[$blank]=_compi_other_run
        zshaddhistory_functions+=( _compi_other_history )
    else
        [[ -z ${BLE_VERSION-} ]] && [[ -o emacs || -o vi ]] || return 0
        (( BASH_VERSINFO[0] > 4 ||
           (BASH_VERSINFO[0] == 4 && BASH_VERSINFO[1] >= 4) )) || return 0
        if type -P pwsh.exe > /dev/null; then
            _compi_other_exe=pwsh.exe
        elif type -P powershell.exe > /dev/null; then
            _compi_other_exe=powershell.exe
        else
            return 0
        fi
        # Enter runs the check, then accepts the line as before.
        local keymap
        for keymap in emacs-standard vi-insert vi-command; do
            bind -m "$keymap" -x '"\C-x\C-]": _compi_other_enter'
            bind -m "$keymap" '"\C-x\C-^": accept-line'
            bind -m "$keymap" '"\C-m": "\C-x\C-]\C-x\C-^"'
        done
    fi
    _compi_other_state=on
}

# Whether the shell itself can run a command word: an alias, function, builtin,
# keyword or file on PATH.
_compi_other_known() {
    if [[ -n ${ZSH_VERSION-} ]]; then
        whence -w -- "$1" > /dev/null 2>&1
    else
        type -t -- "$1" > /dev/null 2>&1
    fi
}

# Whether a line clearly is a PowerShell command and not one of the shell's own:
# $env:NAME, $scope:name, an assignment to $name ($x = 1, $x.Size += 2, $x++), a $name
# the shell does not have that is read the PowerShell way ($x, $x.Count, $x | fl,
# $x -eq 1), [Type]::Member or a [type] cast, a lone string ("got $x") that names no
# command, and a Verb-Noun name, also as (Get-Date).Year, that the shell cannot run
# and PowerShell can. Only builtins run here, except that an unknown name with one of
# PowerShell's verbs asks the tab's PowerShell.
_compi_other_line() {
    local line=$1 word name rest tail quote missing=''
    line=${line#"${line%%[![:space:]]*}"}
    word=${line%%[[:space:]]*}
    case $word in
        '$'[A-Za-z_]*)
            name=${line#?}
            name=${name%%[!A-Za-z0-9_]*}
            rest=${line#?"$name"}
            if [[ -n ${ZSH_VERSION-} ]]; then
                [[ -n ${parameters[$name]+set} ]] || missing=1
            else
                [[ -n ${!name+set} ]] || missing=1
            fi
            case $rest in
                :[A-Za-z_]*)
                    case $name in [Ee][Nn][Vv]) return 0 ;; esac
                    [[ -n $missing ]]
                    return
                    ;;
            esac
            # Past .Member and [index] to the operator.
            tail=${rest#"${rest%%[!A-Za-z0-9_.]*}"}
            case $tail in '['*']'*) tail=${tail#*']'} ;; esac
            tail=${tail#"${tail%%[![:space:]]*}"}
            case $tail in
                '=' | '='[!=~]* | [-+*/%]'='* | '??='* | '++' | '--') return 0 ;;
            esac
            [[ -n $missing ]] || return 1
            case $rest in '' | .[A-Za-z_]* | '['*) return 0 ;; esac
            case $tail in '|'[!'|']* | -[A-Za-z]* | [+*/]' '*) return 0 ;; esac
            return 1
            ;;
        '['[A-Za-z_]*)
            rest=${word#?}
            rest=${rest#"${rest%%[!A-Za-z0-9_.]*}"}
            case $rest in '[]'*) rest=${rest#??} ;; esac
            case $rest in
                ']::'[A-Za-z_]* | ']$'* | ']"'* | "]'"* | ']('* | ']@'* | ']['* | ']'[0-9-]*) ;;
                *) return 1 ;;
            esac
            ! _compi_other_known "$word"
            ;;
        '"'* | "'"*)
            quote=${line%"${line#?}"}
            rest=${line#?}
            [[ $rest == *"$quote"* ]] || return 1
            name=${rest%%"$quote"*}
            rest=${rest#*"$quote"}
            tail=${rest#"${rest%%[![:space:]]*}"}
            case $rest in
                '' | .[A-Za-z_]*) ;;
                *)
                    case $tail in
                        '' | '|'[!'|']* | -[A-Za-z]*' '* | [+*]' '*) ;;
                        *) return 1 ;;
                    esac
                    ;;
            esac
            # The shell would run the string as a command: leave one it can run, and
            # one only a command substitution can settle.
            case $name in *'$('* | *'`'* | *\\) return 1 ;; esac
            if [[ $quote == '"' && $name == *'$'* ]]; then
                eval "name=\"$name\"" 2> /dev/null || return 1
            fi
            ! _compi_other_known "$name"
            ;;
        [A-Za-z]*-[A-Za-z]* | '('*[A-Za-z]-[A-Za-z]*)
            name=${word#"${word%%[!(]*}"}
            name=${name%%')'*}
            case $name in *[!A-Za-z0-9_-]* | *-*-*) return 1 ;; esac
            if [[ -n ${ZSH_VERSION-} ]]; then rest=${name:l}; else rest=${name,,}; fi
            # PowerShell's approved verbs, and the *-Object commands outside them.
            case ${rest%%-*} in
                add | approve | assert | backup | block | build | checkpoint | clear | \
                close | compare | complete | compress | confirm | connect | convert | \
                convertfrom | convertto | copy | debug | deny | deploy | disable | \
                disconnect | dismount | edit | enable | enter | exit | expand | export | \
                find | foreach | format | get | grant | group | hide | import | \
                initialize | install | invoke | join | limit | lock | measure | merge | \
                mount | move | new | open | optimize | out | ping | pop | protect | \
                publish | push | read | receive | redo | register | remove | rename | \
                repair | request | reset | resize | resolve | restart | restore | \
                resume | revoke | save | search | select | send | set | show | skip | \
                sort | split | start | step | stop | submit | suspend | switch | sync | \
                tee | test | trace | unblock | undo | uninstall | unlock | unprotect | \
                unpublish | unregister | update | use | wait | watch | where | write) ;;
                *) return 1 ;;
            esac
            ! _compi_other_known "$name" && _compi_other_exchange find "$name"
            ;;
        *) return 1 ;;
    esac
}

# Bash's Enter. Bash clears the line before a key's function runs, so a PowerShell line
# is printed again after its prompt, kept for history, and replaced by a call of
# _compi_other_run for Bash to accept. Bash draws that call and its line break into
# /dev/null, so the screen shows only what was typed, and HISTCONTROL keeps the call
# (it starts with a space) out of history. Only the first Enter after a prompt is
# checked: later lines of one command and `read -e` are never PowerShell's.
_compi_other_enter() {
    [[ -n ${_compi_other_primary-} && -z ${BLE_VERSION-} ]] || return 0
    _compi_other_primary=
    _compi_other_line "$READLINE_LINE" || return 0
    local prompt=${PS1@P}
    prompt=${prompt##*$'\n'}
    printf '%s%s\n' "${prompt//[$'\001\002']/}" "$READLINE_LINE" >&2
    history -s -- "$READLINE_LINE"
    _compi_other_text=$READLINE_LINE
    _compi_other_histcontrol=${HISTCONTROL-} _compi_other_histcontrol_set=${HISTCONTROL+set}
    HISTCONTROL=ignorespace
    exec {_compi_other_stderr}>&2 2> /dev/null
    READLINE_LINE=' _compi_other_run'
    READLINE_POINT=0
}

# Gives Bash its error output and HISTCONTROL back after _compi_other_enter.
_compi_other_restore() {
    exec 2>&"$_compi_other_stderr" {_compi_other_stderr}>&-
    unset _compi_other_stderr
    if [[ -n $_compi_other_histcontrol_set ]]; then
        HISTCONTROL=$_compi_other_histcontrol
    else
        unset HISTCONTROL
    fi
}

# Zsh's Enter. A PowerShell line is kept for history and shown as text before the
# buffer, and the buffer becomes a no-break space, the name of a function that calls
# _compi_other_run: whatever redraws the line, a prompt theme included, shows what was
# typed. Later lines of one command and `vared` are never PowerShell's.
_compi_other_accept() {
    if [[ $CONTEXT == start && -z $PREBUFFER ]] && _compi_other_line "$BUFFER"; then
        print -s -r -- "$BUFFER"
        _compi_other_text=$BUFFER
        PREDISPLAY=$BUFFER
        BUFFER=$'\xc2\xa0'
    fi
    zle _compi_other_accepted "$@"
}

# Zsh history skips the no-break space that stands for a PowerShell line.
_compi_other_history() {
    [[ $1 != $'\xc2\xa0\n' ]]
}

# Runs the line Enter kept in the tab's PowerShell and returns its exit status. A
# folder PowerShell moved to is this shell's too, where it has one (_compi_cd_target).
_compi_other_run() {
    local text=${_compi_other_text-} result interrupt=''
    _compi_other_text=
    if [[ -n ${ZSH_VERSION-} ]]; then
        setopt localoptions localtraps
    else
        [[ -z ${_compi_other_stderr-} ]] || _compi_other_restore
        # Accepting a line also ends pasting marks until the next prompt.
        printf '\033[?2004l'
        interrupt=$(trap -p INT)
    fi
    # Ctrl+C stops the command in PowerShell; a second one gives PowerShell up.
    _compi_other_stopping=
    trap '_compi_other_stopping=${_compi_other_stopping}x' INT
    _compi_other_exchange run "$text"
    result=$?
    if [[ -z ${ZSH_VERSION-} ]]; then
        trap - INT
        eval "$interrupt"
    fi
    if [[ -n $_compi_other_stopping ]]; then
        # The terminal showed ^C; the prompt starts on a line of its own.
        printf '\n'
        command rm -f -- "$_compi_other_stop"
    fi
    if [[ -n $_compi_other_reply ]]; then
        text=$(_compi_cd_target "$_compi_other_reply") && [[ -d $text ]] && builtin cd -- "$text"
        _compi_other_folder=$PWD
    fi
    return "$result"
}

# Sends one request to the tab's PowerShell, starting it when it is not running, and
# returns the answer's status: `run` runs the text there, in this shell's folder, and
# `find` asks whether PowerShell has a command of that name.
_compi_other_exchange() {
    local kind=$1 text=$2 folder=''
    _compi_other_reply=''
    [[ -z ${_compi_other_pid-} ]] || kill -0 "$_compi_other_pid" 2> /dev/null || _compi_other_close
    if [[ -z ${_compi_other_pid-} ]] && ! _compi_other_start; then
        [[ $kind != run ]] || printf 'compi: PowerShell (%s) did not start\n' "$_compi_other_exe" >&2
        return 127
    fi
    if [[ $kind == run && $PWD != "${_compi_other_folder-}" ]]; then
        folder=$(command wslpath -w "$PWD" 2> /dev/null)
        _compi_other_folder=$PWD
    fi
    printf '\n%s %s\n%s\n%s\n%s end\n' "$_compi_other_nonce" "$kind" "$folder" "$text" \
        "$_compi_other_nonce" >&"$_compi_other_in"
    _compi_other_relay "$kind"
}

# Shows what PowerShell prints until both its output and its error output end with
# the answer, then returns the answer's status. While a command runs, lines typed at
# the keyboard go to it and Ctrl+C stops it. Bash reads up to the answer's first byte
# with a short timeout, which hands over unfinished lines too; Zsh reads blocks.
_compi_other_relay() {
    local LC_ALL=C IFS= kind=$1 chunk='' buffer='' record key code gone=''
    _compi_other_seen='' _compi_other_status=1
    while [[ -z $gone && ${#_compi_other_seen} -lt 2 ]]; do
        if [[ -n ${ZSH_VERSION-} ]]; then
            sysread -i "$_compi_other_out" -t 0.02 chunk
            code=$?
            if (( code == 0 )); then
                buffer+=$chunk
                while [[ $buffer == *$'\036'*$'\037'* ]]; do
                    print -rn -- "${buffer%%$'\036'*}"
                    buffer=${buffer#*$'\036'}
                    record=${buffer%%$'\037'*}
                    buffer=${buffer#*$'\037'}
                    _compi_other_answer "$record"
                done
                if [[ $buffer != *$'\036'* ]]; then
                    print -rn -- "$buffer"
                    buffer=''
                fi
            elif (( code == 5 )); then
                gone=1
            fi
        elif read -r -d $'\036' -t 0.02 -u "$_compi_other_out" chunk; then
            printf '%s' "$chunk"
            read -r -d $'\037' -u "$_compi_other_out" record
            _compi_other_answer "$record"
        else
            code=$?
            printf '%s' "$chunk"
            (( code > 128 )) || gone=1
        fi
        [[ $kind == run ]] || continue
        if [[ -n ${ZSH_VERSION-} ]]; then
            while read -r -t 0 key; do
                print -r -- "$key" >&"$_compi_other_in"
            done
        else
            while read -r -t 0; do
                read -r key || break
                printf '%s\n' "$key" >&"$_compi_other_in"
            done
        fi
        case $_compi_other_stopping in
            x)
                : > "$_compi_other_stop"
                _compi_other_stopping=xx
                ;;
            xxx*) gone=1 _compi_other_status=130 ;;
        esac
    done
    [[ -z $gone ]] || _compi_other_close
    return "$_compi_other_status"
}

# One answer: `O<status>;<folder moved to>` ends the output, `E` the error output.
_compi_other_answer() {
    case $1 in
        "$_compi_other_nonce"E) _compi_other_seen+=E ;;
        "$_compi_other_nonce"O*)
            _compi_other_seen+=O
            _compi_other_reply=${1#"$_compi_other_nonce"O}
            _compi_other_status=${_compi_other_reply%%;*}
            _compi_other_reply=${_compi_other_reply#*;}
            ;;
        *) printf '\036%s\037' "$1" ;;
    esac
}

# Starts the tab's PowerShell on two named pipes, outside the shell's jobs and its
# terminal's Ctrl+C. Its script reads requests on standard input: a `<mark> run|find`
# line, a folder line (empty to stay), the text, and a `<mark> end` line. A command
# runs in a second runspace, which keeps its variables and folder, so the loop can
# stop it when the stop file appears without ending PowerShell. The script leaves when
# its input closes or this shell's process is gone.
_compi_other_start() {
    local directory script
    directory=$(command mktemp -d "${TMPDIR:-/tmp}/compi-other.XXXXXXXX") || return 1
    command mkfifo -m 600 "$directory/in" "$directory/out" || {
        command rm -rf -- "$directory"
        return 1
    }
    _compi_other_nonce=compi$$x$RANDOM$RANDOM
    _compi_other_stop=$directory.stop
    IFS= read -r -d '' script <<'COMPI_OTHER_POWERSHELL'
$utf8 = New-Object Text.UTF8Encoding $false
[Console]::InputEncoding = $utf8
[Console]::OutputEncoding = $utf8
$mark = $env:COMPI_OTHER_MARK
$stop = $env:COMPI_OTHER_STOP
$owner = $env:COMPI_OTHER_OWNER
Remove-Item env:COMPI_OTHER_MARK, env:COMPI_OTHER_STOP, env:COMPI_OTHER_OWNER
$reader = New-Object IO.StreamReader ([Console]::OpenStandardInput()), $utf8
$errors = New-Object IO.StreamWriter ([Console]::OpenStandardError()), $utf8
$out = [Console]::Out
# Errors PowerShell reports join its output, in order.
[Console]::SetError($out)
$runspace = [RunspaceFactory]::CreateRunspace($Host)
$runspace.Open()
$state = $runspace.SessionStateProxy
$state.SetVariable('OutputEncoding', $utf8)
if ($PSVersionTable.PSVersion.Major -ge 7) {
    $shell = [PowerShell]::Create()
    $shell.Runspace = $runspace
    $null = $shell.AddScript('$PSStyle.OutputRendering = ''Ansi''').Invoke()
    $shell.Dispose()
}
function Read-Request {
    $line = $reader.ReadLineAsync()
    while (-not $line.Wait(5000)) {
        if (-not [IO.Directory]::Exists($owner)) { exit }
    }
    if ($null -eq $line.Result) { exit }
    $line.Result
}
while ($true) {
    $line = Read-Request
    if ($line -ne "$mark run" -and $line -ne "$mark find") { continue }
    $folder = Read-Request
    $text = New-Object Text.StringBuilder
    while (($next = Read-Request) -ne "$mark end") {
        if ($text.Length) { $null = $text.Append("`n") }
        $null = $text.Append($next)
    }
    $status = 0
    $place = ''
    $shell = [PowerShell]::Create()
    $shell.Runspace = $runspace
    if ($line -eq "$mark find") {
        $found = $shell.AddScript('[bool] $ExecutionContext.InvokeCommand.GetCommand($args[0], ''All'')').
            AddArgument($text.ToString()).Invoke()
        if (-not $found[0]) { $status = 1 }
    } else {
        if ($folder) {
            $null = $shell.AddCommand('Set-Location').AddParameter('LiteralPath', $folder).Invoke()
            $shell.Commands.Clear()
            $shell.Streams.ClearStreams()
        }
        $before = $state.Path.CurrentFileSystemLocation.ProviderPath
        $native = $state.GetVariable('LASTEXITCODE')
        $state.SetVariable('__compi_ok', $false)
        # The last line records $? the way the next prompt would see it.
        $null = $shell.AddScript($text.ToString() + "`n;`$global:__compi_ok = `$?", $false).
            AddCommand('Out-Default')
        $shell.Commands.Commands[0].MergeMyResults('Error', 'Output')
        $stopped = $false
        try {
            $running = $shell.BeginInvoke()
            while (-not $running.AsyncWaitHandle.WaitOne(100)) {
                if (-not $stopped -and [IO.File]::Exists($stop)) {
                    $stopped = $true
                    $null = $shell.BeginStop($null, $null)
                    # Programs the command started write straight to the pipes.
                    Get-CimInstance Win32_Process -Filter "ParentProcessId = $PID" |
                        Where-Object { $_.Name -ne 'conhost.exe' } |
                        ForEach-Object { & taskkill.exe /T /F /PID $_.ProcessId *> $null }
                }
            }
            $null = $shell.EndInvoke($running)
            if (-not $state.GetVariable('__compi_ok')) {
                $status = 1
                $code = $state.GetVariable('LASTEXITCODE')
                # A program ran if LASTEXITCODE is a new value, even an equal one.
                if ($code -is [int] -and -not [object]::ReferenceEquals($code, $native) -and
                    ($code -band 255)) {
                    $status = $code -band 255
                }
            }
        } catch {
            $status = 1
            $failure = $_.Exception
            if ($failure -is [Management.Automation.MethodInvocationException] -and
                $failure.InnerException) {
                $failure = $failure.InnerException
            }
            if ($failure -is [Management.Automation.IncompleteParseException]) {
                $out.WriteLine($failure.Message)
            } elseif ($failure -isnot [Management.Automation.PipelineStoppedException]) {
                $report = [PowerShell]::Create()
                $report.Runspace = $runspace
                $record = $_
                if ($failure -is [Management.Automation.IContainsErrorRecord]) {
                    $record = $failure.ErrorRecord
                }
                try { $null = $report.AddCommand('Out-Default').Invoke(@($record)) }
                catch { $out.WriteLine("$failure") }
                $report.Dispose()
            }
        }
        if ($stopped) {
            $status = 130
            try { [IO.File]::Delete($stop) } catch { }
        }
        $place = $state.Path.CurrentFileSystemLocation.ProviderPath
        if ($place -eq $before) { $place = '' }
    }
    $shell.Dispose()
    $out.Write("$([char] 30)${mark}O$status;$place$([char] 31)")
    $out.Flush()
    $errors.Write("$([char] 30)${mark}E$([char] 31)")
    $errors.Flush()
}
COMPI_OTHER_POWERSHELL
    exec {_compi_other_in}<> "$directory/in"
    (
        COMPI_OTHER_MARK=$_compi_other_nonce COMPI_OTHER_STOP=$_compi_other_stop \
            COMPI_OTHER_OWNER=/proc/$$ \
            WSLENV="${WSLENV:+$WSLENV:}COMPI_OTHER_MARK:COMPI_OTHER_STOP/p:COMPI_OTHER_OWNER/p" \
            command "$_compi_other_exe" -NoLogo -NoProfile -Command "$script" \
            < "$directory/in" > "$directory/out" 2>&1 {_compi_other_in}>&- &
        printf '%s\n' "$!" > "$directory/pid"
    )
    exec {_compi_other_out}< "$directory/out"
    IFS= read -r _compi_other_pid < "$directory/pid"
    command rm -rf -- "$directory"
}

# Ends the tab's PowerShell; the next PowerShell line starts a new one.
_compi_other_close() {
    [[ -z ${_compi_other_pid-} ]] || kill "$_compi_other_pid" 2> /dev/null
    exec {_compi_other_in}>&- {_compi_other_out}<&-
    unset _compi_other_in _compi_other_out _compi_other_pid _compi_other_folder
}

# Login Bash imports only the prompt functions and reads this file up to here at its
# first prompt (see _compi_prompt_cwd); what follows is for a shell's own startup.
[[ -z ${_compi_other_loading-} ]] || return 0

if [[ -n ${WSL_DISTRO_NAME-} ]]; then
    # Only a path Bash or Zsh cannot find changes, except a moved Windows folder, which
    # wins over the empty one OneDrive leaves at the old place; a cd your startup files
    # define wins over this one.
    cd() {
        local target
        if (( $# == 1 )) && [[ $1 != -* ]]; then
            case $1 in
                /mnt/*/Desktop | /mnt/*/Desktop/* | /mnt/*/Documents | /mnt/*/Documents/* | \
                /mnt/*/Pictures | /mnt/*/Pictures/* | /mnt/*/Music | /mnt/*/Music/* | \
                /mnt/*/Videos | /mnt/*/Videos/*)
                    if target=$(_compi_moved_folder "$1"); then
                        builtin cd -- "$target"
                        return
                    fi
                    ;;
            esac
            if [[ ! -d $1 ]] && target=$(_compi_cd_target "$1") && [[ -d $target ]]; then
                builtin cd -- "$target"
                return
            fi
        fi
        case ${1-} in
            [A-Za-z]:[!\\/]*)
                [[ -d $1 ]] ||
                    printf 'compi: quote a Windows path or use / (the shell drops unquoted backslashes)\n' >&2
                ;;
        esac
        builtin cd "$@"
    }
fi

# From a WSL terminal, Windows starts the GUI-subsystem CLI without any console of its
# own: its output would lose carriage returns and it could never read an answer. Run it
# on pipes instead: its output reaches the terminal through cat, typed lines reach it
# only while it runs, and Ctrl-C ends it. COMPI_CLI_TERMINAL tells it a person is there,
# and COMPI_CLI_COLUMNS how wide the terminal is, for tables and help.
_compi_windows_cli() {
    local directory result
    directory=$(command mktemp -d "${TMPDIR:-/tmp}/compi-cli.XXXXXXXX") || return 1
    command mkfifo -m 600 "$directory/in" "$directory/out" || {
        command rm -rf -- "$directory"
        return 1
    }
    (
        while IFS= read -r line; do printf '%s\n' "$line"; done < /dev/tty > "$directory/in" &
        feeder=$!
        command cat < "$directory/out" &
        relay=$!
        WSLENV="${WSLENV:+$WSLENV:}COMPI_CLI_TERMINAL:COMPI_CLI_COLUMNS" COMPI_CLI_TERMINAL=1 \
            COMPI_CLI_COLUMNS="${COLUMNS:-$(command tput cols 2> /dev/null)}" \
            command "$COMPI_CLI" "$@" < "$directory/in" > "$directory/out" 2>&1 &
        cli=$!
        trap 'kill "$cli" 2> /dev/null' INT
        wait "$cli"
        result=$?
        # A trapped Ctrl-C interrupts wait; collect the CLI's actual end.
        kill -0 "$cli" 2> /dev/null && { wait "$cli"; result=130; }
        kill "$feeder" 2> /dev/null
        wait "$feeder" 2> /dev/null
        wait "$relay"
        exit "$result"
    )
    result=$?
    command rm -rf -- "$directory"
    return "$result"
}

compi() {
    _compi_cli_init || return 1
    if (( $# == 1 )); then
        case "$1" in
            tree) _compi_choose_directory tree; return $? ;;
            z|jump) _compi_choose_directory jump; return $? ;;
        esac
    fi
    if [[ -n ${COMPI_CLI-} ]]; then
        export COMPI_SHELL_CWD="$PWD"
        if [[ -n ${COMPI_CLI_WINDOWS-} && -t 0 && -t 1 && -t 2 ]]; then
            _compi_windows_cli "$@"
        else
            command "$COMPI_CLI" "$@"
        fi
    else
        command compi "$@"
    fi
}

# See _compi_ble_attach; the startup wrappers source this file before the rc.
[[ -z ${BASH_VERSION-} || $- != *i* ]] || alias ble-attach=_compi_ble_attach
