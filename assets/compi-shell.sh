# Source in interactive Bash or Zsh: . /path/to/compi/assets/compi-shell.sh
# Only tree/z/jump are intercepted; other compi commands retain their normal CLI behavior.

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
    # ble.sh runs PROMPT_COMMAND outside the user's prompt state; its hooks sync.
    [[ -n ${BLE_VERSION-} ]] || _compi_prompt_sync "$compi_status"
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
            _compi_prompt_rerun "$1"
        fi
        return 0
    fi
    [[ ! -f $HOME/.compi/prompt/reload ]] || IFS= read -r token < "$HOME/.compi/prompt/reload"
    [[ $token != "${_compi_prompt_token-}" ]] || return 0
    _compi_prompt_token=$token
    _compi_prompt_restore
    [[ ! -f $HOME/.compi/prompt/live.$shell ]] || . "$HOME/.compi/prompt/live.$shell"
    _compi_install_prompt_hook
    _compi_prompt_rerun "$1"
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
        if _compi_prompt_startup; then
            . "$HOME/.compi/prompt/compi.bash"
            _compi_install_prompt_hook
            _compi_prompt_rerun 0
        fi
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
        unset PROMPT_COMMAND
        [[ -z $_compi_saved_prompt_command ]] ||
            eval "${_compi_saved_prompt_command/#declare /declare -g }"
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

# Keep the cwd hook last, so prompt providers capture the command's exit status
# first and a reload never runs an old provider hook after the new one. Array
# entries keep their indices; ble.sh tracks its own entry by index.
_compi_install_prompt_hook() {
    if [[ -n ${ZSH_VERSION-} ]]; then
        precmd_functions=( ${precmd_functions:#_compi_prompt_cwd} _compi_prompt_cwd )
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
        blehook ATTACH!=_compi_prompt_ble_hook
        blehook PRECMD!=_compi_prompt_ble_hook
    fi
}

# Called after the user's shell rc, so existing prompt hooks are preserved.
_compi_enable_prompt_cwd() {
    case $- in *i*) ;; *) return 0 ;; esac
    [[ -z ${BASH_VERSION-} || ${BASH_ALIASES[ble-attach]-} != _compi_ble_attach ]] ||
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

compi() {
    if (( $# == 1 )); then
        case "$1" in
            tree) _compi_choose_directory tree; return $? ;;
            z|jump) _compi_choose_directory jump; return $? ;;
        esac
    fi
    command compi "$@"
}

# See _compi_ble_attach; the startup wrappers source this file before the rc.
[[ -z ${BASH_VERSION-} || $- != *i* ]] || alias ble-attach=_compi_ble_attach
