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

_compi_prompt_cwd() {
    [[ -t 0 || -t 1 || -t 2 ]] || return 0
    [[ ${_compi_last_reported_cwd-} == "$PWD" ]] || _compi_report_cwd
}

# Called after the user's shell rc, so existing prompt hooks are preserved.
_compi_enable_prompt_cwd() {
    case $- in *i*) ;; *) return 0 ;; esac
    if [[ -n ${ZSH_VERSION-} ]]; then
        local hook found=0
        for hook in "${precmd_functions[@]}"; do
            [[ $hook == _compi_prompt_cwd ]] && found=1
        done
        (( found )) || precmd_functions+=( _compi_prompt_cwd )
    else
        case "$(declare -p PROMPT_COMMAND 2>/dev/null)" in
            "declare -a "*)
                local hook found=0
                for hook in "${PROMPT_COMMAND[@]}"; do
                    [[ $hook == _compi_prompt_cwd ]] && found=1
                done
                (( found )) || PROMPT_COMMAND+=( _compi_prompt_cwd )
                ;;
            *)
                case "${PROMPT_COMMAND-}" in
                    *$'\n'_compi_prompt_cwd) ;;
                    *) PROMPT_COMMAND="${PROMPT_COMMAND-}"$'\n''_compi_prompt_cwd' ;;
                esac
                ;;
        esac
    fi
    _compi_prompt_cwd
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
