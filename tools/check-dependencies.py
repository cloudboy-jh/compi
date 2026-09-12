"""Check resolved production/build dependencies, excluding dev-only edges."""

import argparse
import json
import subprocess
import sys


def main():
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument("--target", required=True)
    args = parser.parse_args()
    command = [
        "cargo", "metadata", "--locked", "--format-version", "1",
        "--filter-platform", args.target,
    ]
    result = None
    try:
        result = subprocess.run(command, capture_output=True, check=True)
        metadata = json.loads(result.stdout.decode("utf-8"))
    except (OSError, subprocess.CalledProcessError, json.JSONDecodeError, UnicodeDecodeError) as error:
        if isinstance(error, subprocess.CalledProcessError):
            status = error.returncode
            stderr = error.stderr
        elif result is not None:
            status = result.returncode
            stderr = result.stderr
        else:
            status = "not started"
            stderr = b""
        print(f"Dependency graph resolution failed for target {args.target}", file=sys.stderr)
        print(f"Command: {subprocess.list2cmdline(command)}", file=sys.stderr)
        print(f"Exit status: {status}", file=sys.stderr)
        print(f"Reason: {error}", file=sys.stderr)
        print(f"Stderr:\n{stderr.decode('utf-8', errors='replace').strip() or '(empty)'}", file=sys.stderr)
        raise SystemExit(2) from None
    packages = {package["id"]: package["name"] for package in metadata["packages"]}
    nodes = {node["id"]: node for node in metadata["resolve"]["nodes"]}
    members = {packages[identifier]: identifier for identifier in metadata["workspace_members"]}
    graphics = {"gpui", "raw-window-handle", "winit", "compi-client", "compi-setup"}
    rules = {
        "compi-protocol": graphics | {"portable-pty", "compi-daemon"},
        "compi-client": {"portable-pty", "compi-daemon"},
        "compi-daemon": graphics,
    }
    for name, forbidden in rules.items():
        pending = [members[name]]
        visited = set()
        while pending:
            identifier = pending.pop()
            if identifier in visited:
                continue
            visited.add(identifier)
            if packages[identifier] in forbidden:
                raise SystemExit(f"{name} depends on forbidden package {packages[identifier]}")
            for dependency in nodes[identifier]["deps"]:
                if any(kind["kind"] in (None, "build") for kind in dependency["dep_kinds"]):
                    pending.append(dependency["pkg"])
        print(f"{name}: production/build dependency boundary OK ({args.target})")


if __name__ == "__main__":
    main()
