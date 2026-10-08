#!/usr/bin/env python3
"""PTY smoke tests for the sourced Bash/Zsh shell picker bridge."""

import base64
import os
import pty
import select
import shlex
import shutil
import tempfile
import termios
import time
from pathlib import Path
from urllib.parse import unquote, urlsplit

SCRIPT = Path(__file__).resolve().parents[1] / "assets" / "compi-shell.sh"


def read_until(fd, marker, timeout=5):
    output = bytearray()
    deadline = time.monotonic() + timeout
    while marker not in output:
        remaining = deadline - time.monotonic()
        if remaining <= 0 or not select.select([fd], [], [], remaining)[0]:
            raise AssertionError(f"timed out waiting for {marker!r}: {output!r}")
        try:
            output.extend(os.read(fd, 8192))
        except OSError as exc:
            raise AssertionError(f"PTY closed waiting for {marker!r}: {output!r}") from exc
    return bytes(output)


def exercise(shell, action, response, expected, old_dir, cd_before=None):
    change_directory = (f"builtin cd -- {shlex.quote(str(cd_before))}; "
                        if cd_before is not None else "")
    command = (
        f'. "{SCRIPT}"; {change_directory}compi {action}; '
        'printf "\\nDONE:%s:%s\\n" "$?" "$PWD"'
    )
    pid, fd = pty.fork()
    if pid == 0:
        os.chdir(old_dir)
        os.execv(shell, [shell, "-c", command])
    try:
        first = read_until(fd, f"\x1b]777;compi;{expected['request']}\x07".encode())
        assert b"DONE:" not in first, first
        prefix = b"\x1b]7;"
        before_request = first.split(prefix, 1)
        assert len(before_request) == 2, first
        uri = before_request[1].split(b"\x07", 1)[0].decode()
        assert unquote(urlsplit(uri).path) == str(cd_before or old_dir), uri
        assert first.index(prefix) < first.index(b"\x1b]777;compi;"), first
        deadline = time.monotonic() + 5
        while termios.tcgetattr(fd)[3] & termios.ECHO:
            assert time.monotonic() < deadline, "shell did not enter private picker read"
            time.sleep(0.005)
        os.write(fd, response + b"\n")
        final = read_until(fd, expected["done"].replace(b"\n", b"\r\n"))
        text = first + final
        assert not response or response not in text, "PTY echoed private picker reply"
        if expected.get("osc7") is not None:
            assert prefix in final, final
            uri = final.split(prefix, 1)[1].split(b"\x07", 1)[0].decode()
            assert unquote(urlsplit(uri).path) == expected["osc7"], uri
        else:
            assert prefix not in final, final
    finally:
        os.close(fd)
        waited, status = os.waitpid(pid, 0)
        assert waited == pid and os.waitstatus_to_exitcode(status) == 0, status


def exercise_prompt(shell, target, root):
    if Path(shell).name == "bash":
        args = [shell, "--noprofile", "--norc", "-i", "-c"]
    else:
        args = [shell, "-f", "-i", "-c"]
    command = (
        f'. {shlex.quote(str(SCRIPT))}; '
        '_compi_enable_prompt_cwd; _compi_enable_prompt_cwd; '
        f'builtin cd -- {shlex.quote(str(target))}; '
        '_compi_prompt_cwd; _compi_prompt_cwd; '
        'printf "HOOK_DONE\\n"'
    )
    pid, fd = pty.fork()
    if pid == 0:
        os.chdir(root)
        os.execv(shell, args + [command])
    try:
        text = read_until(fd, b"HOOK_DONE\r\n")
        uris = [part.split(b"\x07", 1)[0].decode()
                for part in text.split(b"\x1b]7;")[1:]]
        assert len(uris) == 2, text
        assert [unquote(urlsplit(uri).path) for uri in uris] == [str(root), str(target)]
    finally:
        os.close(fd)
        _, status = os.waitpid(pid, 0)
        assert os.waitstatus_to_exitcode(status) == 0, status


def exercise_native_prompt(shell, root, home, array_hook=False):
    prompt_dir = home / ".compi" / "prompt"
    prompt_dir.mkdir(parents=True, exist_ok=True)
    initial = prompt_dir / "compi.bash"
    live = prompt_dir / "live.bash"
    reload_token = prompt_dir / "reload"
    def provider_command(name):
        return (f"PROMPT_COMMAND=( [7]={name} )" if array_hook
                else f"PROMPT_COMMAND={name}")
    counts = "${last_status}:${original_runs}:${selected_runs}:${live_runs}:${second_runs}"
    initial.write_text(
        provider_command("compi_selected_prompt") + "\n"
        f"PS1='NATIVE_INITIAL:{counts}> '\n"
    )
    live.unlink(missing_ok=True)
    reload_token.unlink(missing_ok=True)
    rc = root / "native.bashrc"
    rc.write_text(
        f". {shlex.quote(str(SCRIPT))}\n"
        "original_runs=0 selected_runs=0 live_runs=0 second_runs=0 last_status=0\n"
        "compi_original_prompt() {\n"
        "    last_status=$?\n"
        "    ((original_runs += 1))\n"
        "    if [[ ${fixture_mutate-} == 1 ]]; then\n"
        "        PROMPT_COMMAND=( [5]=compi_second_prompt )\n"
        "    fi\n"
        '    return "$last_status"\n'
        "}\n"
        "compi_selected_prompt() { last_status=$?; ((selected_runs += 1)); return \"$last_status\"; }\n"
        "compi_live_prompt() { last_status=$?; ((live_runs += 1)); return \"$last_status\"; }\n"
        "compi_second_prompt() { last_status=$?; ((second_runs += 1)); return \"$last_status\"; }\n"
        + provider_command("compi_original_prompt") + "\n"
        + f"PS1='NATIVE_ORIGINAL:{counts}> '\n"
        + 'if _compi_prompt_startup; then . "$HOME/.compi/prompt/compi.bash"; fi\n'
        + "_compi_enable_prompt_cwd\n"
    )
    pid, fd = pty.fork()
    if pid == 0:
        os.chdir(root)
        os.execv(shell, [shell, "--noprofile", "--rcfile", str(rc), "-i"])
    try:
        first = read_until(fd, b"NATIVE_INITIAL:0:0:1:0:0> ")
        assert b"NATIVE_ORIGINAL:" not in first, first
        os.write(fd, b"false\n")
        read_until(fd, b"NATIVE_INITIAL:1:0:2:0:0> ")
        live.write_text(
            provider_command("compi_live_prompt") + "\n"
            f"PS1='NATIVE_LIVE:{counts}> '\n"
        )
        reload_token.write_text("live\n")
        os.write(fd, b"false\n")
        read_until(fd, b"NATIVE_LIVE:1:0:2:1:0> ")
        live.unlink()
        reload_token.write_text("off\n")
        os.write(fd, b"false\n")
        read_until(fd, b"NATIVE_ORIGINAL:1:1:2:1:0> ")
        os.write(fd, b"fixture_mutate=1\n")
        read_until(fd, b"NATIVE_ORIGINAL:0:2:2:1:0> ")
        os.write(fd, b"false\n")
        read_until(fd, b"NATIVE_ORIGINAL:1:2:2:1:1> ")
        os.write(fd, b"exit 0\n")
        _, status = os.waitpid(pid, 0)
        pid = None
        assert os.waitstatus_to_exitcode(status) == 0, status
    finally:
        os.close(fd)
        if pid is not None:
            os.waitpid(pid, 0)
        initial.unlink(missing_ok=True)
        live.unlink(missing_ok=True)
        reload_token.unlink(missing_ok=True)


def exercise_native_context(shell, root, array_hook=False):
    calls = root / "native-context-calls"
    rc = root / "native-context.bashrc"
    hook = ("PROMPT_COMMAND=( [2]=first [5]=second )" if array_hook
            else "PROMPT_COMMAND='first; second'")
    callbacks = ""
    for name, status in (("first", 7), ("second", 9)):
        callbacks += (
            f"{name}() {{\n"
            '    local status=$? argument=$_ pipeline=( "${PIPESTATUS[@]}" )\n'
            f"    printf '{name}|%s|%s|%s\\n' \"$status\" \"$argument\" "
            f'"${{pipeline[*]}}" >> {shlex.quote(str(calls))}\n'
            f"    return {status}\n"
            "}\n"
        )
    rc.write_text(
        f". {shlex.quote(str(SCRIPT))}\n" + callbacks + hook + "\n"
        + "PS1='CONTEXT_READY> '\n_compi_enable_prompt_cwd\n"
    )
    pid, fd = pty.fork()
    if pid == 0:
        os.chdir(root)
        os.execv(shell, [shell, "--noprofile", "--rcfile", str(rc), "-i"])
    try:
        read_until(fd, b"CONTEXT_READY> ")
        for command, status, argument, pipeline in (
            ("printf -v fixture_dummy '%s' LASTARG; false | true", 0, "LASTARG", "1 0"),
            ("set -o pipefail; printf -v fixture_dummy '%s' LASTARG; false | true",
             1, "LASTARG", "1 0"),
            ("set +o pipefail; printf -v fixture_dummy '%s' LASTARG; false",
             1, "false", "1"),
        ):
            calls.write_text("")
            os.write(fd, command.encode() + b"\n")
            read_until(fd, b"CONTEXT_READY> ")
            expected = [f"first|{status}|{argument}|{pipeline}"]
            expected.append(f"second|{status}|{argument}|{pipeline}" if array_hook
                            else "second|7|first|7")
            assert calls.read_text().splitlines() == expected, calls.read_text()
        os.write(fd, b"exit 0\n")
        _, status = os.waitpid(pid, 0)
        pid = None
        assert os.waitstatus_to_exitcode(status) == 0, status
    finally:
        os.close(fd)
        if pid is not None:
            os.waitpid(pid, 0)


def exercise_ble_prompt(shell, ble_script, root, home, array_hook=False):
    """Exercise real ble.sh startup, completion and reload prompt dispatch."""
    prompt_dir = home / ".compi" / "prompt"
    prompt_dir.mkdir(parents=True, exist_ok=True)
    initial = prompt_dir / "compi.bash"
    live = prompt_dir / "live.bash"
    reload_token = prompt_dir / "reload"
    initial.write_text(
        "PS1='COMPI_INITIAL:${last_status}:${prompt_runs}> '\n"
    )
    live.unlink(missing_ok=True)
    reload_token.unlink(missing_ok=True)
    rc = root / "ble.bashrc"
    hook = ("PROMPT_COMMAND=( compi_fixture_prompt )" if array_hook
            else "PROMPT_COMMAND=compi_fixture_prompt")
    rc.write_text(
        f". {shlex.quote(str(SCRIPT))}\n"
        f"source -- {shlex.quote(str(ble_script))} --attach=none || exit 1\n"
        "bleopt highlight_syntax= highlight_filename= highlight_variable=\n"
        "prompt_runs=0 last_status=0\n"
        "compi_fixture_prompt() {\n"
        "    last_status=$?\n"
        "    ((prompt_runs += 1))\n"
        '    return "$last_status"\n'
        "}\n"
        f"{hook}\n"
        "PS1='COMPI_ORIGINAL:${last_status}:${prompt_runs}> '\n"
        "compi_fixture_complete() { printf '\\nCOMPLETED:%s\\n' \"$1\"; }\n"
        "complete -W completion-proof compi_fixture_complete\n"
        "ble-attach || exit 1\n"
        "[[ -n ${BLE_VERSION-} && -n ${_ble_attached-} && "
        "-n ${_ble_edit_attached-} ]] || exit 1\n"
        "_compi_enable_prompt_cwd\n"
    )
    pid, fd = pty.fork()
    if pid == 0:
        os.chdir(root)
        os.environ["TERM"] = "xterm-256color"
        os.environ.pop("VSCODE_INJECTION", None)
        os.environ.pop("kitty_bash_inject", None)
        os.environ.pop("ghostty_bash_inject", None)
        os.environ.pop("__ghostty_bash_flags", None)
        os.execv(shell, [shell, "--noprofile", "--rcfile", str(rc), "-i"])
    try:
        first = read_until(fd, b"COMPI_INITIAL:0:1> ", timeout=20)
        assert b"COMPI_ORIGINAL:" not in first, first
        os.write(fd, b"false\n")
        read_until(fd, b"COMPI_INITIAL:1:2> ")
        os.write(fd, b"compi_fixture_complete complet\t")
        read_until(fd, b"completion-proof")
        os.write(fd, b"\n")
        completed = read_until(fd, b"COMPI_INITIAL:0:3> ")
        assert b"COMPLETED:completion-proof\r\n" in completed, completed
        live.write_text(
            "PS1='COMPI_LIVE:${last_status}:${prompt_runs}> '\n"
        )
        reload_token.write_text("live\n")
        os.write(fd, b"false\n")
        read_until(fd, b"COMPI_LIVE:1:4> ")
        live.unlink()
        reload_token.write_text("off\n")
        os.write(fd, b"false\n")
        read_until(fd, b"COMPI_ORIGINAL:1:5> ")
        os.write(fd, b"exit 0\n")
        _, status = os.waitpid(pid, 0)
        pid = None
        assert os.waitstatus_to_exitcode(status) == 0, status
    finally:
        os.close(fd)
        if pid is not None:
            os.waitpid(pid, 0)
        initial.unlink(missing_ok=True)
        live.unlink(missing_ok=True)
        reload_token.unlink(missing_ok=True)


def main():
    with tempfile.TemporaryDirectory(prefix="compi shell ") as temp:
        root = Path(temp)
        # Prompt settings in the real home must not load into these shells.
        home = root / "home"
        home.mkdir()
        os.environ["HOME"] = str(home)
        target = root / "folder with space π\n"
        target.mkdir()
        nested = root / "nested"
        nested.mkdir()
        encoded = base64.b64encode(os.fsencode(target))
        literal = root / "literal $(touch PWNED);`exit`"
        literal.mkdir()
        for name in ("bash", "zsh"):
            shell = shutil.which(name)
            if shell is None:
                print(f"SKIP {name} (unavailable)")
                continue
            for action, request in (("tree", "tree"), ("z", "jump"), ("jump", "jump")):
                exercise(shell, action, encoded, {
                    "request": request,
                    "done": f"DONE:0:{target}\n".encode(),
                    "osc7": str(target),
                }, str(root))
            exercise(shell, "tree", base64.b64encode(os.fsencode(literal)), {
                "request": "tree", "done": f"DONE:0:{literal}\n".encode(),
                "osc7": str(literal),
            }, str(root))
            assert not (root / "PWNED").exists(), "directory name ran as shell code"
            exercise(shell, "tree", b"", {
                "request": "tree", "done": f"DONE:0:{root}\n".encode(), "osc7": None,
            }, str(root))
            for invalid in (base64.b64encode(b"relative"),
                            base64.b64encode(os.fsencode(target) + b"\x00"), b"@@@"):
                exercise(shell, "jump", invalid, {
                    "request": "jump", "done": f"DONE:1:{root}\n".encode(),
                    "osc7": None,
                }, str(root))
            for action, request in (("tree", "tree"), ("z", "jump")):
                exercise(shell, action, b"", {
                    "request": request,
                    "done": f"DONE:0:{nested}\n".encode(),
                    "osc7": None,
                }, str(root), cd_before=nested)
            exercise_prompt(shell, target, root)
            if name == "bash":
                for array_hook in (False, True):
                    exercise_native_prompt(shell, root, home, array_hook)
                    exercise_native_context(shell, root, array_hook)
            print(f"PASS {name} picker replies and idempotent OSC7 prompt hooks")
        ble_script = Path(os.environ.get(
            "COMPI_TEST_BLE", "/usr/local/share/blesh/ble.sh"
        ))
        bash = shutil.which("bash")
        if bash and ble_script.is_file():
            for array_hook in (False, True):
                exercise_ble_prompt(bash, ble_script, root, home, array_hook)
            print("PASS ble.sh single prompt dispatch, status, completion and live reload")
        else:
            print("SKIP ble.sh (set COMPI_TEST_BLE to an installed ble.sh)")


if __name__ == "__main__":
    main()
