# Compi's hook for the shells agents run their commands in (BASH_ENV). Compi starts
# every agent it runs in Linux with it, and a Windows agent's commands reach it through
# Compi's agent shell, so a command an agent runs (`bash -c COMMAND`) lands where the
# chat is, as one typed in a Compi terminal there: a PowerShell command runs in
# PowerShell, and `cd` takes a Windows path. Your own BASH_ENV, if you have one, runs
# first. Only the shell an agent started acts on its command; the scripts it runs, and
# the shells they start, never see this file.

if [[ -n ${COMPI_USER_BASH_ENV+set} ]]; then
    if [[ -n $COMPI_USER_BASH_ENV ]]; then export BASH_ENV=$COMPI_USER_BASH_ENV; else unset BASH_ENV; fi
    unset COMPI_USER_BASH_ENV
    if [[ -n ${BASH_ENV-} ]]; then . "$BASH_ENV"; fi
else
    unset BASH_ENV
fi

# The command this shell was started to run (its -c argument), from its arguments: bash
# sets BASH_EXECUTION_STRING only after reading this file.
_compi_agent_command() {
    local args argument command='' with_c='' skip=''
    mapfile -d '' -t args < "/proc/$$/cmdline" 2> /dev/null || return 1
    for argument in "${args[@]:1}"; do
        if [[ -n $skip ]]; then
            skip=''
            continue
        fi
        case $argument in
            --) continue ;;
            -o | +o | -O | +O | --rcfile | --init-file) skip=1 ;;
            --*) ;;
            -* | +*) [[ $argument != -*c* ]] || with_c=1 ;;
            *)
                command=$argument
                break
                ;;
        esac
    done
    [[ -n $with_c ]] || return 1
    printf '%s' "$command"
}

# A Windows agent writes Windows paths its Git Bash would read: Git Bash's paths of
# Windows drives (/c/Users/…) anywhere, and a drive's path given to a variable
# (SNAPSHOT_FILE='C:\Users\…'). They become their place in WSL (/mnt/c/Users/…), for
# every drive WSL mounts. Other Windows paths stay as written: they may be a Windows
# program's arguments.
_compi_agent_paths() {
    local root letters='' drive text rest out='' match path
    local assigned='=(["'"'"'])([A-Za-z]):[\/]([^"'"'"']*)'
    root=$(command wslpath -u 'C:\' 2> /dev/null) || {
        printf '%s' "$1"
        return
    }
    root=${root%c/}
    root=${root%C/}
    for drive in "$root"?; do
        [[ -d $drive && ${drive#"$root"} == [a-zA-Z] ]] && letters+=${drive#"$root"}
    done
    if [[ -z $letters ]]; then
        printf '%s' "$1"
        return
    fi
    letters+=${letters^^}
    text=$(printf '%s' "$1" | command sed -E "s#(^|[[:space:]\"'=<>(;|&:])/([$letters])(/|\$|[[:space:]\"';|&)])#\\1$root\\L\\2\\E\\3#g")
    rest=$text
    while [[ $rest =~ $assigned ]]; do
        match=${BASH_REMATCH[0]}
        out+=${rest%%"$match"*}
        rest=${rest#*"$match"}
        drive=${BASH_REMATCH[2],,}
        path=${BASH_REMATCH[3]//\\//}
        if [[ $letters == *"$drive"* ]]; then
            out+="=${BASH_REMATCH[1]}$root$drive/$path"
        else
            out+=$match
        fi
    done
    printf '%s' "$out$rest"
}

# Whether `text` is a PowerShell command, as a Compi terminal decides for a typed line.
# Only a line that can be one (it starts with `$`, `[`, a quote or a Verb-Noun word)
# looks for PowerShell: the search path's Windows folders are slow to search.
_compi_agent_is_powershell() {
    local line=${1#"${1%%[![:space:]]*}"}
    [[ ${COMPI_OTHER_SHELL_COMMANDS-} != 0 ]] || return 1
    case ${line%%[[:space:]]*} in
        '$'* | '['* | '"'* | "'"* | *[A-Za-z]-[A-Za-z]*) ;;
        *) return 1 ;;
    esac
    if [[ -z ${_compi_other_exe-} ]]; then
        if type -P pwsh.exe > /dev/null; then
            _compi_other_exe=pwsh.exe
        elif type -P powershell.exe > /dev/null; then
            _compi_other_exe=powershell.exe
        else
            return 1
        fi
    fi
    _compi_other_line "$line"
}

# Runs PowerShell command `text` in this shell's folder, as a Compi terminal runs one
# typed in Bash, and returns its status.
_compi_agent_powershell() {
    local result
    _compi_other_exchange run "$1"
    result=$?
    _compi_other_close
    return "$result"
}

if [[ $- != *i* ]] && _compi_agent_text=$(_compi_agent_command); then
    # Compi's shell functions: `cd` that takes Windows paths in WSL, `compi`, and the
    # PowerShell companion a terminal uses for commands from the other shell.
    . "${BASH_SOURCE[0]%/*}/compi-shell.sh"
    # A command a harness wraps (Claude Code's `eval 'COMMAND'`) is checked too.
    eval() {
        if (( $# == 1 )) && _compi_agent_is_powershell "$1"; then
            _compi_agent_powershell "$1"
        else
            builtin eval "$@"
        fi
    }
    if [[ -n ${COMPI_AGENT_MSYS-} ]]; then
        # Compi's agent shell: the Windows agent's folder, then its command with its
        # Windows drives' paths where WSL has them.
        unset COMPI_AGENT_MSYS
        if [[ -n ${COMPI_AGENT_CWD-} ]]; then
            _compi_agent_folder=$(command wslpath -u "$COMPI_AGENT_CWD" 2> /dev/null) &&
                builtin cd -- "$_compi_agent_folder" 2> /dev/null
        fi
        unset COMPI_AGENT_CWD _compi_agent_folder
        _compi_agent_text=$(_compi_agent_paths "$_compi_agent_text")
        if _compi_agent_is_powershell "$_compi_agent_text"; then
            _compi_agent_powershell "$_compi_agent_text"
        else
            builtin eval "$_compi_agent_text"
        fi
        exit
    fi
    if _compi_agent_is_powershell "$_compi_agent_text"; then
        _compi_agent_powershell "$_compi_agent_text"
        exit
    fi
    unset _compi_agent_text
fi
