# Monitor's collector inside a WSL distribution. The Windows daemon pipes this file into
# `wsl.exe --distribution D --exec /bin/sh -s MODE [ARGUMENTS]`; it needs only POSIX sh, awk
# and /proc, so no Linux binary is shipped.
#
#   stream               a header, then one sample every 2 seconds until the daemon kills it
#   inspect PID START BOOT_TIME
#                        the process's state, working directory and arguments
#   quit PID START TERM|KILL BOOT_TIME
#                        signals the process, if it is still the one that started at START
#
# START is a start time in Unix seconds, as the stream reports it: boot time plus the
# process's start in clock ticks over CLK_TCK, rounded down.
# BOOT_TIME is the stream header's epoch, not a fresh /proc/stat reading: WSL clock
# synchronization can shift btime while the same processes keep running.
#
# The stream writes tab-separated lines, names and command lines with tabs and line breaks
# turned to spaces:
#   H  CLK_TCK PAGE_SIZE UID BOOT_TIME LOGICAL_CPUS HOST_NAME OS CPU_MODEL      once, first
#   S  UPTIME                                     begins a sample (seconds since boot)
#   c  LINE                                       a cpu or cpuN line of /proc/stat
#   m  NAME KIB                                   a line of /proc/meminfo
#   k  DEVICE SECTORS_READ SECTORS_WRITTEN       a line of /proc/diskstats
#   n  INTERFACE ADDRESS RECEIVED SENT            a line of /proc/net/dev and its address
#   F  DEVICE MOUNT_POINT TYPE TOTAL_KIB AVAILABLE_KIB
#                                                 a mounted block device; every 30th sample
#   d  PID START_TICKS SESSION UID USER NAME EXECUTABLE COMMAND
#                                                 a process first seen, before its p line
#   p  PID PARENT STATE START_TICKS USER_TICKS SYSTEM_TICKS RSS_PAGES THREADS
#   E                                             ends the sample
#
# `inspect` writes NUL-terminated fields: `gone`, or `ok`, the state letter, the working
# directory (empty when unreadable) and then each argument. `quit` writes one line: `sent`,
# `gone` or `denied`.

set -f
hz=$(getconf CLK_TCK 2>/dev/null) || hz=100
page=$(getconf PAGESIZE 2>/dev/null) || page=4096

# Sets `state` (the letter) and `start` (Unix seconds) for process $1; fails once it is gone.
started() {
    stat=$(cat "/proc/$1/stat" 2>/dev/null) || return 1
    rest=${stat##*)}
    set -- $rest
    [ $# -ge 20 ] || return 1
    state=$1
    shift 19
    start=$((btime + $1 / hz))
}

case $1 in
stream)
    exec awk -v hz="$hz" -v page="$page" -v uid="$(id -u)" '
function first(path,    text) {
    text = ""
    if ((getline text < path) <= 0) text = ""
    close(path)
    return text
}
function clean(text) {
    gsub(/[\t\r\n]/, " ", text)
    return text
}
function users(    line, field) {
    while ((getline line < "/etc/passwd") > 0) {
        split(line, field, ":")
        name[field[3]] = field[1]
    }
    close("/etc/passwd")
}
function header(    line, field, host, os, model, cpus, btime) {
    host = first("/proc/sys/kernel/hostname")
    while ((getline line < "/etc/os-release") > 0)
        if (line ~ /^PRETTY_NAME=/) {
            os = substr(line, 13)
            gsub(/"/, "", os)
        }
    close("/etc/os-release")
    while ((getline line < "/proc/cpuinfo") > 0)
        if (model == "" && line ~ /^model name/) {
            model = line
            sub(/^[^:]*: */, "", model)
        }
    close("/proc/cpuinfo")
    cpus = 0
    while ((getline line < "/proc/stat") > 0) {
        if (line ~ /^cpu[0-9]/) cpus++
        else if (line ~ /^btime /) {
            split(line, field, " ")
            btime = field[2]
        }
    }
    close("/proc/stat")
    printf "H\t%s\t%s\t%s\t%s\t%s\t%s\t%s\t%s\n", hz, page, uid, btime, cpus, clean(host), clean(os), clean(model)
}
function volumes(    line, field, type, list, command) {
    list = ""
    while ((getline line < "/proc/mounts") > 0) {
        split(line, field, " ")
        if (field[1] ~ "^/dev/" && field[2] ~ "^[-A-Za-z0-9/._+]+$" && !(field[2] in type)) {
            type[field[2]] = field[3]
            list = list " " field[2]
        }
    }
    close("/proc/mounts")
    if (list == "") return
    command = "df -Pk" list " 2>/dev/null"
    while ((command | getline line) > 0) {
        split(line, field, " ")
        if (field[6] in type)
            printf "F\t%s\t%s\t%s\t%s\t%s\n", field[1], field[6], type[field[6]], field[2], field[4]
    }
    close(command)
}
function details(pid, f, comm,    path, line, field, owner, command, exe, arguments) {
    owner = ""
    path = "/proc/" pid "/status"
    while ((getline line < path) > 0)
        if (line ~ /^Uid:/) {
            split(line, field, " ")
            owner = field[3]
        }
    close(path)
    if (owner != "" && !(owner in name)) users()
    command = "cd /proc/" pid " 2>/dev/null && printf \"%s\\n\" \"$(readlink exe 2>/dev/null)\" && tr \"\\000\\t\\n\\r\" \"    \" < cmdline 2>/dev/null; echo"
    exe = ""
    arguments = ""
    if ((command | getline exe) > 0) command | getline arguments
    close(command)
    printf "d\t%s\t%s\t%s\t%s\t%s\t%s\t%s\t%s\n", pid, f[20], f[4], owner, clean(name[owner]), clean(comm), clean(exe), clean(substr(arguments, 1, 4096))
}
function sample(    line, field, at, pid, stat, rest, end, open, f, key, command) {
    split(first("/proc/uptime"), field, " ")
    printf "S\t%s\n", field[1]
    while ((getline line < "/proc/stat") > 0)
        if (line ~ /^cpu/) printf "c\t%s\n", line
    close("/proc/stat")
    while ((getline line < "/proc/meminfo") > 0)
        if (line ~ /^(MemTotal|MemAvailable|Cached|Buffers|SReclaimable|SwapTotal|SwapFree):/) {
            split(line, field, " ")
            printf "m\t%s\t%s\n", substr(field[1], 1, length(field[1]) - 1), field[2]
        }
    close("/proc/meminfo")
    while ((getline line < "/proc/diskstats") > 0) {
        split(line, field, " ")
        if (field[3] !~ /^(loop|ram|zram)/) printf "k\t%s\t%s\t%s\n", field[3], field[6], field[10]
    }
    close("/proc/diskstats")
    while ((getline line < "/proc/net/dev") > 0) {
        at = index(line, ":")
        if (at == 0) continue
        key = substr(line, 1, at - 1)
        gsub(/ /, "", key)
        split(substr(line, at + 1), field, " ")
        if (key != "lo")
            printf "n\t%s\t%s\t%s\t%s\n", key, first("/sys/class/net/" key "/address"), field[1], field[9]
    }
    close("/proc/net/dev")
    if (samples % 30 == 0) volumes()
    samples++
    command = "ls /proc"
    while ((command | getline pid) > 0) {
        if (pid !~ /^[0-9]+$/) continue
        stat = first("/proc/" pid "/stat")
        end = 0
        rest = stat
        while ((at = index(rest, ")")) > 0) {
            end += at
            rest = substr(rest, at + 1)
        }
        open = index(stat, "(")
        if (open == 0 || end == 0 || split(substr(stat, end + 2), f, " ") < 22) continue
        key = pid " " f[20]
        now[key] = 1
        if (!(key in seen)) details(pid, f, substr(stat, open + 1, end - open - 1))
        printf "p\t%s\t%s\t%s\t%s\t%s\t%s\t%s\t%s\n", pid, f[2], f[1], f[20], f[12], f[13], f[22], f[18]
    }
    close(command)
    for (key in seen) if (!(key in now)) delete seen[key]
    for (key in now) {
        seen[key] = 1
        delete now[key]
    }
    print "E"
}
BEGIN {
    users()
    header()
    for (;;) {
        sample()
        fflush()
        system("sleep 2")
    }
}'
    ;;
inspect | quit)
    case $2 in '' | *[!0-9]*) exit 2 ;; esac
    case $3 in '' | *[!0-9]*) exit 2 ;; esac
    if [ "$1" = inspect ]; then btime=$4; else btime=$5; fi
    case $btime in '' | *[!0-9]*) exit 2 ;; esac
    if ! started "$2" || [ "$start" != "$3" ]; then
        if [ "$1" = inspect ]; then printf 'gone\0'; else echo gone; fi
        exit 0
    fi
    if [ "$1" = inspect ]; then
        printf 'ok\0%s\0%s\0' "$state" "$(readlink "/proc/$2/cwd" 2>/dev/null)"
        cat "/proc/$2/cmdline" 2>/dev/null
    else
        case $4 in TERM | KILL) ;; *) exit 2 ;; esac
        if kill -s "$4" "$2" 2>/dev/null; then
            echo sent
        elif [ -e "/proc/$2" ]; then
            echo denied
        else
            echo gone
        fi
    fi
    ;;
*)
    exit 2
    ;;
esac
