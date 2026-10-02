#!/bin/bash
set -euo pipefail

# The development probe is deliberately external to the distributable bundle.
# Requires the logged-in desktop session and Python 3 provided by macos-14.
# Usage: bash tools/smoke-macos-bundle.sh <artifact.dmg> <compi-probe> [<new.app.zip> <signed-metadata.json>]
if (($# != 2 && $# != 4)); then
    printf 'Usage: bash tools/smoke-macos-bundle.sh <artifact.dmg> <compi-probe> [<new.app.zip> <signed-metadata.json>]\n' >&2
    exit 2
fi
if [[ $(uname -s) != Darwin || $(uname -m) != arm64 ]]; then
    printf 'This smoke requires a native ARM64 macOS desktop session.\n' >&2
    exit 1
fi

export COMPI_SMOKE_CYCLE_SCRIPT="$(cd -- "$(dirname -- "${BASH_SOURCE[0]}")" && pwd -P)/smoke-update-cycle.py"
python3 - "$@" <<'PYTHON'
import ctypes
import json
import hashlib
import os
from pathlib import Path
import shutil
import signal
import plistlib
import subprocess
import sys
import tempfile
import time
import uuid


def run(*args, **kwargs):
    return subprocess.run(args, check=True, text=True, capture_output=True, timeout=30, **kwargs)


dmg = Path(sys.argv[1]).resolve(strict=True)
probe = Path(sys.argv[2]).resolve(strict=True)
update_artifact = Path(sys.argv[3]).resolve(strict=True) if len(sys.argv) == 5 else None
update_metadata = Path(sys.argv[4]).resolve(strict=True) if len(sys.argv) == 5 else None
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
sentinel = root / "data" / "preservation-sentinel.txt"
sentinel.write_text(uuid.uuid4().hex)
sentinel_bytes = sentinel.read_bytes()
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


def workspace(target=None):
    result = run(str(probe), "--existing", "--instance", target or instance, "workspace", env=environment)
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


def attached_workspace(target=None):
    snapshot = workspace(target)
    if (snapshot["initialized"] and snapshot["sessions"]
            and any(surface["status"] == "running" and surface["attached"]
                    for surface in snapshot["surfaces"])
            and len(owned_pids(gui)) == 1 and len(owned_pids(daemon)) == 1):
        return snapshot
    return None


def launch(target=None):
    arguments = ["/usr/bin/open", "-n", "--arch", "arm64", "-a", str(app),
                 "--stdout", str(log), "--stderr", str(log)]
    for name in ("COMPI_DATA_DIR", "COMPI_RUNTIME_DIR", "HOME", "SHELL"):
        arguments.extend(["--env", f"{name}={environment[name]}"])
    # Do not pass --working-directory on reconnect: that intentionally opens
    # another terminal instead of restoring the existing surface.
    arguments.extend(["--args", "--instance", target or instance])
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
    scenarios = ["dmg-copy-launch-reconnect"]
    legacy_zip = os.environ.get("COMPI_SMOKE_LEGACY_APP_ZIP")
    legacy_workspace = None
    if legacy_zip:
        # Drag-replace migration: an older copied bundle runs and persists data first.
        legacy_instance = "legacy-" + uuid.uuid4().hex[:19]
        run("/usr/bin/ditto", "-x", "-k", legacy_zip, str(app.parent))
        legacy_version = plistlib.loads((app / "Contents/Info.plist").read_bytes())["CFBundleShortVersionString"]
        launch(legacy_instance)
        wait_for(f"legacy {legacy_version} GUI, daemon and attached running PTY",
                 lambda: attached_workspace(legacy_instance))
        stop_owned(gui)
        stop_owned(daemon)
        legacy_workspace = root / "data" / f"workspace-{legacy_instance}-v1.json"
        legacy_bytes = legacy_workspace.read_bytes()
        shutil.rmtree(app)
    run("/usr/bin/ditto", str(mount / "Compi.app"), str(app))
    run("/usr/bin/hdiutil", "detach", str(mount))
    mounted = False
    run("/usr/bin/codesign", "--verify", "--deep", "--strict", str(app))
    helper = str(app / "Contents/MacOS/compi-update-worker")
    for executable in (gui, daemon, helper):
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
    version = plistlib.loads((app / "Contents/Info.plist").read_bytes())["CFBundleShortVersionString"]
    full_payload = dmg.parent / f"Compi-{version}-macOS-arm64.app.zip"
    with full_payload.open("rb") as stream:
        payload_hash = hashlib.file_digest(stream, "sha256").hexdigest()
    if sentinel.read_bytes() != sentinel_bytes:
        raise RuntimeError("Copied-bundle lifecycle changed managed data")
    if legacy_workspace:
        if legacy_workspace.read_bytes() != legacy_bytes:
            raise RuntimeError("Bundle replacement changed the legacy workspace data")
        scenarios.append("legacy-bundle-replacement")
    update_cycle = None
    if update_artifact:
        driver = probe.parent / "compi-update-smoke"
        if not driver.is_file():
            raise RuntimeError("Build compi-client --example compi-update-smoke for real GUI handoff qualification")
        (app.parent / f".{app.name}.compi-smoke-owned.json").write_text(
            json.dumps({"root": str(app.resolve()), "nonce": uuid.uuid4().hex, "artifact_sha256": payload_hash}))
        stop_owned(gui)
        run(str(probe), "--instance", instance, "shutdown", env=environment)
        wait_for("old smoke daemon shutdown", lambda: not owned_pids(daemon))
        cycle = Path(os.environ["COMPI_SMOKE_CYCLE_SCRIPT"])
        cycle_evidence = dmg.parent.parent / "distribution-smoke" / f"update-cycle-macos-{uuid.uuid4().hex[:12]}"
        subprocess.run([sys.executable, str(cycle), "--root", str(app),
                        "--artifact", str(update_artifact), "--metadata", str(update_metadata),
                        "--probe", str(probe), "--driver", str(driver),
                        "--evidence", str(cycle_evidence)],
                       env=environment, check=True, timeout=300)
        runs = list(cycle_evidence.glob("cycle-*/evidence.json"))
        if len(runs) != 1:
            raise RuntimeError("Successful update smoke must produce exactly one evidence file")
        update_cycle = json.loads(runs[0].read_text())
    qualification = {
        "schema": 1, "platform": "macos-aarch64", "version": version,
        "daemon_protocol": 15, "qualified_daemons": [version],
        "artifact_sha256": payload_hash, "installer_scenarios": scenarios,
    }
    if update_cycle:
        qualification["update_cycle"] = update_cycle
    (dmg.parent / "qualification-macos-aarch64.json").write_text(json.dumps(qualification, indent=2) + "\n")
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
    if succeeded:
        shutil.rmtree(root)
    else:
        print(f"Failure retained disposable root and prior usable bundle: {root}", file=sys.stderr)

print("PASS: DMG copy launched through LaunchServices, discovered its bundled daemon, "
      "and reconnected to the same running terminal; all test-owned processes stopped.")
print("This is lifecycle evidence, not visual, physical-input, IME, or Gatekeeper qualification.")
PYTHON
