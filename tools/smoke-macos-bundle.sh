#!/bin/bash
set -euo pipefail

# The development probe is deliberately external to the distributable bundle.
# Requires the logged-in desktop session and Python 3 provided by macos-14.
# Usage: bash tools/smoke-macos-bundle.sh <artifact.dmg> <compi-probe>
if (($# != 2)); then
    printf 'Usage: bash tools/smoke-macos-bundle.sh <artifact.dmg> <compi-probe>\n' >&2
    exit 2
fi
if [[ $(uname -s) != Darwin || $(uname -m) != arm64 ]]; then
    printf 'This smoke requires a native ARM64 macOS desktop session.\n' >&2
    exit 1
fi

python3 - "$@" <<'PYTHON'
import ctypes
import json
import os
from pathlib import Path
import shutil
import signal
import subprocess
import sys
import tempfile
import time
import uuid


def run(*args, **kwargs):
    return subprocess.run(args, check=True, text=True, capture_output=True, timeout=30, **kwargs)


dmg = Path(sys.argv[1]).resolve(strict=True)
probe = Path(sys.argv[2]).resolve(strict=True)
if not os.access(probe, os.X_OK):
    raise SystemExit(f"Probe is not executable: {probe}")
# A short private runtime path also stays below the Unix socket path limit.
root = Path(tempfile.mkdtemp(prefix="compi-smoke-", dir="/tmp")).resolve()
instance = "smoke-" + uuid.uuid4().hex[:20]
mount = root / "volume"
app = root / "Copied Applications" / "Compi.app"
gui = str(app / "Contents/MacOS/compi")
daemon = str(app / "Contents/MacOS/compi-daemon")
log = root / "client.log"
mount.mkdir()
for name in ("data", "runtime", "home"):
    (root / name).mkdir(mode=0o700)
environment = os.environ.copy()
environment.update({
    "COMPI_DATA_DIR": str(root / "data"),
    "COMPI_RUNTIME_DIR": str(root / "runtime"),
    "HOME": str(root / "home"),
    "SHELL": "/bin/bash",
})
# Match the executable's actual kernel path, not a process-name substring.
# A unique copied bundle makes every process at these two paths test-owned.
libproc = ctypes.CDLL("/usr/lib/libproc.dylib")
libproc.proc_pidpath.argtypes = [ctypes.c_int, ctypes.c_void_p, ctypes.c_uint32]
libproc.proc_pidpath.restype = ctypes.c_int


def executable_path(pid):
    buffer = ctypes.create_string_buffer(4096)
    if libproc.proc_pidpath(pid, buffer, len(buffer)) <= 0:
        return None
    return os.fsdecode(buffer.value)


def owned_pids(executable):
    pids = run("/bin/ps", "-axo", "pid=").stdout.split()
    return [int(pid) for pid in pids if executable_path(int(pid)) == executable]


def stop_owned(executable):
    pids = owned_pids(executable)
    for pid in pids:
        if executable_path(pid) == executable:
            try:
                os.kill(pid, signal.SIGTERM)
            except ProcessLookupError:
                pass
    deadline = time.monotonic() + 10
    while owned_pids(executable):
        if time.monotonic() >= deadline:
            raise RuntimeError(f"Test-owned processes did not terminate: {executable}")
        time.sleep(0.1)


def workspace():
    result = run(str(probe), "--instance", instance, "workspace", env=environment)
    return json.loads(result.stdout)


def wait_for(description, condition):
    deadline = time.monotonic() + 45
    last_error = None
    while time.monotonic() < deadline:
        try:
            result = condition()
            if result:
                return result
        except (subprocess.SubprocessError, ValueError, OSError) as error:
            last_error = error
        time.sleep(0.2)
    raise RuntimeError(f"Timed out waiting for {description}; last error: {last_error}")


def attached_workspace():
    snapshot = workspace()
    if (snapshot["initialized"] and snapshot["sessions"]
            and any(surface["status"] == "running" and surface["attached"]
                    for surface in snapshot["surfaces"])
            and len(owned_pids(gui)) == 1 and len(owned_pids(daemon)) == 1):
        return snapshot
    return None


def launch():
    arguments = ["/usr/bin/open", "-n", "--arch", "arm64", "-a", str(app),
                 "--stdout", str(log), "--stderr", str(log)]
    for name in ("COMPI_DATA_DIR", "COMPI_RUNTIME_DIR", "HOME", "SHELL"):
        arguments.extend(["--env", f"{name}={environment[name]}"])
    # Do not pass --working-directory on reconnect: that intentionally opens
    # another terminal instead of restoring the existing surface.
    arguments.extend(["--args", "--instance", instance])
    run(*arguments, env=environment)


mounted = False
succeeded = False
try:
    run("/usr/bin/hdiutil", "attach", str(dmg), "-readonly", "-nobrowse",
        "-mountpoint", str(mount))
    mounted = True
    if not (mount / "Applications").is_symlink() or os.readlink(mount / "Applications") != "/Applications":
        raise RuntimeError("DMG lacks the /Applications installation symlink")
    app.parent.mkdir()
    run("/usr/bin/ditto", str(mount / "Compi.app"), str(app))
    run("/usr/bin/hdiutil", "detach", str(mount))
    mounted = False
    run("/usr/bin/codesign", "--verify", "--deep", "--strict", str(app))
    for executable in (gui, daemon):
        if not os.access(executable, os.X_OK):
            raise RuntimeError(f"Copied bundle lost executable permission: {executable}")
        if run("/usr/bin/lipo", "-archs", executable).stdout.strip() != "arm64":
            raise RuntimeError(f"Copied executable is not ARM64: {executable}")
    run(daemon, "--check-system", env=environment)

    # No daemon-start call: only the copied GUI may discover/start its sibling.
    launch()
    before = wait_for("LaunchServices GUI, sibling daemon and attached running PTY", attached_workspace)
    daemon_pid = owned_pids(daemon)[0]
    running = {surface["id"]: surface["process_lifetime_id"]
               for surface in before["surfaces"] if surface["status"] == "running"}
    print("Initial copied-bundle workspace:", json.dumps(before), flush=True)

    stop_owned(gui)

    def detached_workspace():
        snapshot = workspace()
        return snapshot if all(not surface["attached"] for surface in snapshot["surfaces"]) else None

    detached = wait_for("GUI detach while the daemon stays available", detached_workspace)
    if owned_pids(daemon) != [daemon_pid] or detached["server_id"] != before["server_id"]:
        raise RuntimeError("Closing the copied GUI replaced or stopped its daemon")
    launch()
    after = wait_for("reopened GUI reattaching to its existing running PTY", attached_workspace)
    restored = {surface["id"]: surface["process_lifetime_id"]
                for surface in after["surfaces"] if surface["status"] == "running"}
    if (after["server_id"] != before["server_id"]
            or after["server_generation"] != before["server_generation"]
            or owned_pids(daemon) != [daemon_pid] or restored != running):
        raise RuntimeError("Reconnect did not preserve the daemon and terminal process lifetimes")
    print("Reconnected copied-bundle workspace:", json.dumps(after), flush=True)
    succeeded = True
finally:
    # Never use killall, a bundle identifier, or the user's default instance.
    cleanup_errors = []
    try:
        stop_owned(gui)
    except Exception as error:
        cleanup_errors.append(str(error))
    try:
        if owned_pids(daemon):
            run(str(probe), "--instance", instance, "shutdown", env=environment)
            wait_for("test daemon shutdown", lambda: not owned_pids(daemon))
    except Exception as error:
        cleanup_errors.append(str(error))
    if mounted or os.path.ismount(mount):
        try:
            run("/usr/bin/hdiutil", "detach", str(mount))
        except Exception as error:
            cleanup_errors.append(str(error))
    if not succeeded or cleanup_errors:
        for logfile in [log, *sorted((root / "data").glob("*.log"))]:
            if logfile.is_file():
                print(f"--- {logfile} ---\n{logfile.read_text(errors='replace')[-16000:]}", file=sys.stderr)
    if cleanup_errors:
        # Preserve the owned files if anything might still be using them.
        raise RuntimeError(f"Smoke cleanup failed; retained {root}: {cleanup_errors}")
    shutil.rmtree(root)

print("PASS: DMG copy launched through LaunchServices, discovered its bundled daemon, "
      "and reconnected to the same running terminal; all test-owned processes stopped.")
print("This is lifecycle evidence, not visual, physical-input, IME, or Gatekeeper qualification.")
PYTHON
