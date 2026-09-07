"""Check resolved production/build dependencies, excluding dev-only edges."""

import argparse
import json
import subprocess


def main():
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument("--target", required=True)
    args = parser.parse_args()
    metadata = json.loads(
        subprocess.check_output(
            [
                "cargo", "metadata", "--locked", "--format-version", "1",
                "--filter-platform", args.target,
            ],
            text=True,
        )
    )
    packages = {package["id"]: package["name"] for package in metadata["packages"]}
    nodes = {node["id"]: node for node in metadata["resolve"]["nodes"]}
    members = {packages[identifier]: identifier for identifier in metadata["workspace_members"]}
    graphics = {"gpui", "raw-window-handle", "winit", "compi-gpui", "compi-setup"}
    os_runtime = {"windows", "windows-sys", "windows-core", "portable-pty", "winresource"}
    rules = {
        "compi-protocol": graphics
        | os_runtime
        | {"compi-terminal", "compi-platform", "compi-client", "compi-daemon"},
        "compi-terminal": graphics
        | os_runtime
        | {"compi-platform", "compi-client", "compi-daemon"},
        "compi-platform": graphics
        | {"portable-pty", "winresource", "compi-terminal", "compi-client", "compi-daemon"},
        "compi-client": graphics | {"portable-pty", "compi-terminal", "compi-daemon"},
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
