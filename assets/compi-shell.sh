# Source in interactive Bash or Zsh: . /path/to/compi/assets/compi-shell.sh
# Only tree/z/jump are intercepted; other compi commands retain their normal CLI behavior.

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
          (digits % 4 == 0 && padding != 0) || value != 0 )) ||
       [[ $directory != /* || ! -d $directory ]]; then
        printf 'compi: invalid directory selection\n' > /dev/tty
        return 1
    fi
    builtin cd -- "$directory" || return 1
    _compi_report_cwd
}

# From a WSL terminal, Windows starts the GUI-subsystem CLI without any console of its
# own: its output would lose carriage returns and it could never read an answer. Run it
# on pipes instead: its output reaches the terminal through cat, typed lines reach it
# only while it runs, and Ctrl-C ends it. COMPI_CLI_TERMINAL tells it a person is there.
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
        WSLENV="${WSLENV:+$WSLENV:}COMPI_CLI_TERMINAL" COMPI_CLI_TERMINAL=1 \
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
