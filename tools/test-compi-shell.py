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


def exercise_prompt(shell, target, root, array_hook=False):
    if Path(shell).name == "bash":
        preset = ("PROMPT_COMMAND=( 'printf ORIGINAL' )" if array_hook
                  else "PROMPT_COMMAND='printf ORIGINAL'")
        check = ('[[ ${PROMPT_COMMAND[0]} == "printf ORIGINAL" '
                 '&& ${#PROMPT_COMMAND[@]} == 2 ]]' if array_hook else
                 '[[ $PROMPT_COMMAND == *ORIGINAL* ]]')
        args = [shell, "--noprofile", "--norc", "-i", "-c"]
    else:
        preset = "original_prompt() { :; }; precmd_functions=( original_prompt )"
        check = ('[[ ${precmd_functions[1]} == original_prompt '
                 '&& ${#precmd_functions[@]} == 2 ]]')
        args = [shell, "-f", "-i", "-c"]
    command = (
        f'. {shlex.quote(str(SCRIPT))}; {preset}; '
        '_compi_enable_prompt_cwd; _compi_enable_prompt_cwd; '
        f'builtin cd -- {shlex.quote(str(target))}; '
        '_compi_prompt_cwd; _compi_prompt_cwd; '
        f'{check}; printf "PRESERVED:%s\\nHOOK_DONE\\n" "$?"'
    )
    pid, fd = pty.fork()
    if pid == 0:
        os.chdir(root)
        os.execv(shell, args + [command])
    try:
        text = read_until(fd, b"HOOK_DONE\r\n")
        assert b"PRESERVED:0\r\n" in text, text
        uris = [part.split(b"\x07", 1)[0].decode()
                for part in text.split(b"\x1b]7;")[1:]]
        assert len(uris) == 2, text
        assert [unquote(urlsplit(uri).path) for uri in uris] == [str(root), str(target)]
    finally:
        os.close(fd)
        _, status = os.waitpid(pid, 0)
        assert os.waitstatus_to_exitcode(status) == 0, status


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
                exercise_prompt(shell, target, root, array_hook=True)
            print(f"PASS {name} picker replies and idempotent OSC7 prompt hooks")


if __name__ == "__main__":
    main()
