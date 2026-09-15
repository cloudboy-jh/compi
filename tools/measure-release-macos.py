#!/usr/bin/env python3
import argparse
import csv
import hashlib
import json
import os
import platform
import re
import signal
import subprocess
import sys
import time
from datetime import datetime, timezone
from pathlib import Path

METRICS = (
    "first_window_ms",
    "first_terminal_ms",
    "ready_for_input_ms",
    "input_to_render_ms",
)
RESOURCE_FIELDS = (
    "private_bytes",
    "resident_bytes",
    "virtual_bytes",
    "working_set_bytes",
    "handles",
    "file_descriptors",
    "threads",
)


def arguments():
    parser = argparse.ArgumentParser(description="Measure a native macOS Compi release build")
    parser.add_argument("--binary-directory", type=Path, default=Path("target/release"))
    parser.add_argument("--samples", type=int, default=10)
    parser.add_argument(
        "--mode",
        action="append",
        choices=("warm", "cold", "empty"),
        dest="modes",
    )
    parser.add_argument("--font-family", default="Menlo")
    parser.add_argument("--font-size", type=float, default=14.0)
    parser.add_argument("--line-height", type=float, default=1.35)
    parser.add_argument("--theme", default="dark-glass")
    parser.add_argument("--confirm-physical-display", action="store_true")
    args = parser.parse_args()
    if not 1 <= args.samples <= 100:
        parser.error("--samples must be between 1 and 100")
    args.modes = args.modes or ["warm", "cold", "empty"]
    return args


def run_probe(probe, instance, *command, capture=False):
    result = subprocess.run(
        [str(probe), "--instance", instance, *command],
        check=False,
        stdout=subprocess.PIPE if capture else subprocess.DEVNULL,
        stderr=subprocess.PIPE,
        text=True,
    )
    if result.returncode:
        detail = result.stderr.strip() or result.stdout.strip()
        raise RuntimeError(f"compi-probe {' '.join(command)} failed: {detail}")
    return result.stdout.strip() if capture else ""


def wait_daemon(probe, instance, timeout=15):
    deadline = time.monotonic() + timeout
    while time.monotonic() < deadline:
        result = subprocess.run(
            [str(probe), "--instance", instance, "workspace"],
            stdout=subprocess.DEVNULL,
            stderr=subprocess.DEVNULL,
            check=False,
        )
        if result.returncode == 0:
            return
        time.sleep(0.1)
    raise RuntimeError(f"daemon instance {instance} did not become ready")


def stop_daemon(probe, instance):
    run_probe(probe, instance, "shutdown")


def wait_line(path, predicate, process=None, timeout=24):
    deadline = time.monotonic() + timeout
    while time.monotonic() < deadline:
        if path.exists():
            for line in reversed(path.read_text(errors="replace").splitlines()):
                if predicate(line):
                    return line
        if process is not None and process.poll() is not None:
            raise RuntimeError(f"process {process.pid} exited before emitting required metrics")
        time.sleep(0.1)
    raise RuntimeError(f"timed out waiting for metrics in {path}")


def wait_startup_metric(startup_log, sample, metric, process):
    pattern = re.compile(
        rf"(?:^| )sample={re.escape(sample)} .*?metric={re.escape(metric)} value_ms=(\d+)"
    )
    line = wait_line(startup_log, lambda candidate: pattern.search(candidate), process, 20)
    return int(pattern.search(line).group(1))


def parse_fields(line):
    return dict(re.findall(r"(?:^| )([a-z_]+)=([^ ]*)", line))


def wait_resource(data_dir, process_kind, process, sample, expected_sessions=None):
    path = data_dir / f"{process_kind}-resource-{process.pid}.log"

    def matches(line):
        fields = parse_fields(line)
        return fields.get("sample") == sample and (
            expected_sessions is None
            or fields.get("sessions") == str(expected_sessions)
        )

    return wait_line(path, matches, process)


def start_process(path, args, environment):
    env = os.environ.copy()
    env.update(environment)
    return subprocess.Popen(
        [str(path), *args],
        env=env,
        stdout=subprocess.DEVNULL,
        stderr=subprocess.DEVNULL,
        start_new_session=True,
    )


def stop_process(process):
    if process.poll() is not None:
        return
    process.terminate()
    try:
        process.wait(timeout=5)
    except subprocess.TimeoutExpired:
        os.killpg(process.pid, signal.SIGKILL)
        process.wait(timeout=5)


def result_row(sample, startup, process_kind, process, resource_line, timings=None):
    fields = parse_fields(resource_line)
    row = {
        "sample": sample,
        "startup": startup,
        "process_kind": process_kind,
        "process_id": process.pid,
        "first_window_ms": None,
        "first_terminal_ms": None,
        "ready_for_input_ms": None,
        "input_to_render_ms": None,
        "private_bytes": None,
        "resident_bytes": None,
        "virtual_bytes": None,
        "working_set_bytes": None,
        "handles": None,
        "file_descriptors": None,
        "threads": None,
        "gpu_dedicated_bytes": None,
        "gpu_shared_bytes": None,
        "resource_log": resource_line,
    }
    if timings:
        row.update(timings)
    for field in RESOURCE_FIELDS:
        if field in fields:
            row[field] = int(fields[field])
    return row


def client_sample(
    client,
    data_dir,
    startup_log,
    startup,
    sample,
    instance,
    presentation,
    empty=False,
    sessions=1,
):
    environment = {
        "COMPI_PERF_LOG": "1",
        "COMPI_PERF_SAMPLE": sample,
        "COMPI_PERF_STARTUP_KIND": startup,
        "COMPI_PERF_SESSION_COUNT": str(sessions),
    }
    if empty:
        environment["COMPI_PERF_EMPTY_WINDOW"] = "1"
    else:
        environment["COMPI_PERF_READY_PROBE"] = "1"
    process = start_process(client, ["--instance", instance, *presentation], environment)
    try:
        timings = {"first_window_ms": wait_startup_metric(startup_log, sample, "first_window_frame_ms", process)}
        if not empty:
            timings.update(
                {
                    "first_terminal_ms": wait_startup_metric(startup_log, sample, "first_terminal_frame_ms", process),
                    "ready_for_input_ms": wait_startup_metric(startup_log, sample, "ready_for_input_ms", process),
                    "input_to_render_ms": wait_startup_metric(startup_log, sample, "input_to_render_ms", process),
                }
            )
        resource = wait_resource(data_dir, "client", process, sample, 0 if empty else sessions)
        return result_row(sample, startup, "client", process, resource, timings)
    finally:
        stop_process(process)


def daemon_process(instance):
    result = subprocess.run(
        ["ps", "-axo", "pid=,command="],
        check=True,
        stdout=subprocess.PIPE,
        text=True,
    )
    matches = []
    marker = f"--instance {instance}"
    for line in result.stdout.splitlines():
        pid, _, command = line.strip().partition(" ")
        if command.endswith(marker) and "compi-daemon" in command:
            matches.append(int(pid))
    if len(matches) != 1:
        raise RuntimeError(f"expected one daemon process for instance {instance}, found {len(matches)}")
    return matches[0]


def process_handle(pid):
    class ProcessHandle:
        def __init__(self, process_id):
            self.pid = process_id

        def poll(self):
            try:
                os.kill(self.pid, 0)
                return None
            except ProcessLookupError:
                return 0

    return ProcessHandle(pid)


def percentile(values, percent):
    ordered = sorted(values)
    return ordered[max(0, (len(ordered) * percent + 99) // 100 - 1)]


def command_output(*command):
    result = subprocess.run(command, check=False, stdout=subprocess.PIPE, stderr=subprocess.STDOUT, text=True)
    return result.stdout.strip() if result.returncode == 0 else None


def display_context():
    result = subprocess.run(
        ["system_profiler", "SPDisplaysDataType", "-json"],
        check=False,
        stdout=subprocess.PIPE,
        stderr=subprocess.DEVNULL,
        text=True,
    )
    if result.returncode:
        return None
    try:
        return json.loads(result.stdout)
    except json.JSONDecodeError:
        return None


def main():
    args = arguments()
    if sys.platform != "darwin":
        raise SystemExit("measure-release-macos.py must run on macOS")

    binary_dir = args.binary_directory.resolve()
    client = binary_dir / "compi"
    daemon = binary_dir / "compi-daemon"
    probe = binary_dir / "examples" / "compi-probe"
    for path in (client, daemon, probe):
        if not path.is_file():
            raise SystemExit(f"required measurement binary was not found: {path}")

    data_dir = Path.home() / "Library" / "Application Support" / "Compi"
    output_dir = data_dir / "measurements"
    output_dir.mkdir(parents=True, exist_ok=True)
    started = datetime.now(timezone.utc)
    run_id = f"release-{started.strftime('%Y%m%d-%H%M%S')}-{os.getpid()}"
    instance_base = f"m{started.strftime('%m%d%H%M%S')}{os.getpid()}"
    startup_log = data_dir / "client-startup.log"
    rows = []
    presentation = [
        "--font-family",
        args.font_family,
        "--font-size",
        str(args.font_size),
        "--line-height",
        str(args.line_height),
        "--theme",
        args.theme,
    ]

    if "empty" in args.modes:
        for index in range(1, args.samples + 1):
            sample = f"{run_id}-empty-{index:02d}"
            rows.append(
                client_sample(
                    client,
                    data_dir,
                    startup_log,
                    "empty",
                    sample,
                    f"{instance_base}-e{index:02d}",
                    presentation,
                    True,
                    0,
                )
            )

    if "warm" in args.modes:
        instance = f"{instance_base}-w"
        daemon_sample = f"{run_id}-warm-daemon"
        daemon_handle = start_process(
            daemon,
            ["--instance", instance],
            {"COMPI_PERF_LOG": "1", "COMPI_PERF_SAMPLE": daemon_sample},
        )
        wait_daemon(probe, instance)
        try:
            client_sample(
                client,
                data_dir,
                startup_log,
                "warm",
                f"{run_id}-warmup",
                instance,
                presentation,
            )
            for index in range(1, args.samples + 1):
                sample = f"{run_id}-warm-{index:02d}"
                rows.append(
                    client_sample(
                        client,
                        data_dir,
                        startup_log,
                        "warm",
                        sample,
                        instance,
                        presentation,
                    )
                )
        finally:
            stop_daemon(probe, instance)
            daemon_handle.wait(timeout=10)

        for sessions in (1, 2, 4):
            instance = f"{instance_base}-s{sessions}"
            sample = f"{run_id}-sessions-{sessions}"
            daemon_sample = f"{sample}-daemon"
            daemon_handle = start_process(
                daemon,
                ["--instance", instance],
                {"COMPI_PERF_LOG": "1", "COMPI_PERF_SAMPLE": daemon_sample},
            )
            wait_daemon(probe, instance)
            try:
                rows.append(
                    client_sample(
                        client,
                        data_dir,
                        startup_log,
                        f"marginal-{sessions}",
                        sample,
                        instance,
                        presentation,
                        sessions=sessions,
                    )
                )
                resource = wait_resource(data_dir, "daemon", daemon_handle, daemon_sample, sessions)
                rows.append(result_row(daemon_sample, f"marginal-{sessions}", "daemon", daemon_handle, resource))
            finally:
                stop_daemon(probe, instance)
                daemon_handle.wait(timeout=10)

    if "cold" in args.modes:
        for index in range(1, args.samples + 1):
            instance = f"{instance_base}-c{index:02d}"
            sample = f"{run_id}-cold-{index:02d}"
            try:
                rows.append(
                    client_sample(
                        client,
                        data_dir,
                        startup_log,
                        "cold",
                        sample,
                        instance,
                        presentation,
                    )
                )
                pid = daemon_process(instance)
                daemon_handle = process_handle(pid)
                resource = wait_resource(data_dir, "daemon", daemon_handle, sample)
                rows.append(result_row(sample, "cold", "daemon", daemon_handle, resource))
            finally:
                stop_daemon(probe, instance)

    qualified = (
        args.confirm_physical_display
        and args.samples >= 10
        and all(mode in args.modes for mode in ("warm", "cold", "empty"))
    )
    commit = command_output("git", "rev-parse", "HEAD")
    for row in rows:
        row["run_id"] = run_id

    csv_path = output_dir / f"{run_id}.csv"
    fieldnames = ["run_id", *[key for key in rows[0] if key != "run_id"]]
    with csv_path.open("w", newline="") as output:
        writer = csv.DictWriter(output, fieldnames=fieldnames)
        writer.writeheader()
        writer.writerows(rows)

    context = {
        "run_id": run_id,
        "qualified_physical_display_run": qualified,
        "physical_display_confirmed_by_operator": args.confirm_physical_display,
        "samples_per_mode": args.samples,
        "modes": args.modes,
        "binary_directory": str(binary_dir),
        "build_profile": binary_dir.name,
        "font_family": args.font_family,
        "font_size": args.font_size,
        "line_height": args.line_height,
        "theme": args.theme,
        "platform": platform.platform(),
        "macos": platform.mac_ver()[0],
        "machine": platform.machine(),
        "model": command_output("sysctl", "-n", "hw.model"),
        "cpu": command_output("sysctl", "-n", "machdep.cpu.brand_string"),
        "displays": display_context(),
        "commit": commit,
        "rustc": command_output("rustc", "--version"),
        "binaries": {
            "client_sha256": hashlib.file_digest(client.open("rb"), "sha256").hexdigest(),
            "daemon_sha256": hashlib.file_digest(daemon.open("rb"), "sha256").hexdigest(),
            "probe_sha256": hashlib.file_digest(probe.open("rb"), "sha256").hexdigest(),
        },
    }
    context_path = output_dir / f"{run_id}-environment.json"
    context_path.write_text(json.dumps(context, indent=2) + "\n")

    print(f"Measurement CSV: {csv_path}")
    print(f"Environment JSON: {context_path}")
    print(f"Qualified physical-display run: {str(qualified).lower()}")
    for startup in args.modes:
        matching = [row for row in rows if row["process_kind"] == "client" and row["startup"] == startup]
        for metric in METRICS:
            values = [row[metric] for row in matching if row[metric] is not None]
            if values:
                print(f"{startup} {metric} p50={percentile(values, 50)} p95={percentile(values, 95)} worst={max(values)}")


if __name__ == "__main__":
    main()
