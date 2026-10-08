#!/usr/bin/env python3
"""Exercise signed B->C on a script-owned copy with real GUI handoff/receipts."""
import argparse
import base64
import ctypes
import hashlib
import json
import os
from pathlib import Path
import plistlib
import re
import shlex
import shutil
import signal
import subprocess
import sys
import tempfile
import time
import uuid


def digest(path):
    with path.open('rb') as stream:
        return hashlib.file_digest(stream, 'sha256').hexdigest()


def main():
    parser = argparse.ArgumentParser(description=__doc__)
    for name in ('root', 'artifact', 'metadata', 'probe', 'driver', 'evidence'):
        parser.add_argument('--' + name, type=Path, required=True)
    args = parser.parse_args()
    if sys.platform not in ('win32', 'darwin'):
        parser.error('Run on native Windows x64/WSL2 or Apple Silicon macOS')
    root = args.root.resolve(strict=True)
    marker = root.parent / f'.{root.name}.compi-smoke-owned.json'
    owned = json.loads(marker.read_text())
    if owned.get('root') != str(root) or not owned.get('nonce'):
        parser.error('Root is not a script-owned disposable package copy')
    windows = sys.platform == 'win32'
    platform = 'windows-x86_64' if windows else 'macos-aarch64'
    if windows:
        before_selection = json.loads((root / 'selection.json').read_bytes())
        current = before_selection['version']
        payload = root / 'versions' / current
        gui = payload / 'compi.exe'
        worker = root / 'compi-update-worker.exe'
        daemon_path = payload / 'compi-daemon.exe'
    else:
        current = plistlib.loads((root / 'Contents/Info.plist').read_bytes())['CFBundleShortVersionString']
        payload = root / 'Contents/MacOS'
        gui = payload / 'compi'
        worker = payload / 'compi-update-worker'
        daemon_path = payload / 'compi-daemon'
    signed = json.loads(args.metadata.read_text())
    candidate = json.loads(base64.b64decode(signed['payload'], validate=True))
    if candidate['platform'] != platform or current not in candidate['qualified_daemon_versions'] or candidate['daemon_protocol'] != 18:
        parser.error('Compatible cycle requires signed native qualification of the running B daemon')
    if tuple(map(int, candidate['version'].split('.'))) <= tuple(map(int, current.split('.'))):
        parser.error('C must be genuinely newer than B')
    run_root = args.evidence.resolve() / ('cycle-' + uuid.uuid4().hex[:12])
    run_root.mkdir(parents=True)
    profile = run_root / 'profile'
    profile.mkdir(mode=0o700)
    environment = os.environ.copy()
    if windows:
        environment['LOCALAPPDATA'] = str(profile)
        data = profile / 'Compi'
    else:
        data = profile / 'data'
        # Unix socket paths are limited to ~104 bytes; keep the runtime dir short.
        runtime = Path(tempfile.mkdtemp(prefix='compi-cycle-', dir='/tmp')).resolve()
        environment.update(COMPI_DATA_DIR=str(data), COMPI_RUNTIME_DIR=str(runtime))
    data.mkdir(mode=0o700)
    sentinels = {}
    for filename in ('config.toml', 'external-project.txt'):
        path = (data if filename != 'external-project.txt' else profile) / filename
        content = ('# smoke data retention ' + uuid.uuid4().hex
                   + '\nversion = 1\n[updates]\ncheck_for_updates = false\n') if filename == 'config.toml' else uuid.uuid4().hex
        path.write_text(content)
        sentinels[path] = path.read_bytes()
    driver = payload / ('compi-update-smoke.exe' if windows else 'compi-update-smoke')
    shutil.copy2(args.driver, driver)
    # Mac diagnostic instrumentation changes the copied seal. Production signing
    # must be qualified before this driver is added; never claim this is Gatekeeper proof.
    instance = 'upd-' + uuid.uuid4().hex[:16]
    nonce = uuid.uuid4().hex[:12]
    launched = []
    log = (run_root / 'gui.log').open('wb')

    def run(*command, check=True, timeout=60):
        result = subprocess.run([str(value) for value in command], env=environment,
                                capture_output=True, text=True, timeout=timeout)
        with (run_root / 'commands.log').open('a', encoding='utf8') as output:
            output.write(json.dumps([str(value) for value in command]) + '\n' + result.stdout + result.stderr + '\n')
        if check and result.returncode:
            raise RuntimeError(f'Command failed ({result.returncode}): {command}: {result.stderr}')
        return result

    theme_source = profile / 'custom-theme-source.json'
    theme_source.write_text(json.dumps({
        'name': 'Update qualification', 'author': 'Compi native smoke',
        'themes': [{'name': 'Update qualification dark', 'appearance': 'dark',
                    'style': {'background': '#18222D', 'text': '#E6EDF3',
                              'terminal.background': '#18222D',
                              'terminal.foreground': '#E6EDF3'}}]
    }))
    theme_ids = json.loads(run(driver, 'import-theme', theme_source).stdout)
    config_path = data / 'config.toml'
    config_path.write_text(config_path.read_text()
                           + '\n[appearance]\ntheme = ' + json.dumps(theme_ids[0])
                           + '\ntransparent_background = false\n'
                           + '\n[font]\nsize = 15.5\n')
    sentinels[config_path] = config_path.read_bytes()
    sentinels[theme_source] = theme_source.read_bytes()
    for theme_path in (data / 'themes').rglob('*.json'):
        sentinels[theme_path] = theme_path.read_bytes()

    def wait(description, condition, timeout=45):
        deadline = time.monotonic() + timeout
        last = None
        while time.monotonic() < deadline:
            try:
                value = condition()
                if value:
                    return value
            except (OSError, subprocess.SubprocessError, ValueError, RuntimeError) as error:
                last = error
            time.sleep(0.15)
        raise RuntimeError(f'Timed out: {description}; last error={last}')

    def workspace():
        return json.loads(run(args.probe, '--existing', '--instance', instance, 'workspace').stdout)

    def hosts():
        return json.loads(run(driver, 'hosts', instance).stdout)

    def identity(snapshot):
        return (snapshot['server_id'], snapshot['server_generation'],
                tuple((surface['id'], surface['process_lifetime_id'], surface['status'])
                      for surface in snapshot['surfaces']),
                tuple(session['id'] for session in snapshot['sessions']))

    def attached():
        value = workspace()
        return value if len(value['surfaces']) == 1 and value['surfaces'][0]['attached'] and value['surfaces'][0]['status'] == 'running' else None

    def launch():
        process = subprocess.Popen([str(gui), '--instance', instance], env=environment, stdout=log, stderr=log)
        launched.append(process)
        return wait('test GUI attached to the original running shell', attached)

    def close_gui():
        current_hosts = hosts()
        for host in current_hosts:
            pid = host['pid']
            if windows:
                user32 = ctypes.windll.user32
                callback_type = ctypes.WINFUNCTYPE(ctypes.c_bool, ctypes.c_void_p, ctypes.c_void_p)
                user32.GetWindowThreadProcessId.argtypes = [ctypes.c_void_p, ctypes.POINTER(ctypes.c_ulong)]
                user32.PostMessageW.argtypes = [ctypes.c_void_p, ctypes.c_uint, ctypes.c_size_t, ctypes.c_ssize_t]
                user32.EnumWindows.argtypes = [callback_type, ctypes.c_void_p]
                def close(hwnd, _):
                    owner = ctypes.c_ulong()
                    user32.GetWindowThreadProcessId(hwnd, ctypes.byref(owner))
                    if owner.value == pid:
                        user32.PostMessageW(hwnd, 0x0010, 0, 0)
                    return True
                callback = callback_type(close)
                user32.EnumWindows(callback, 0)
            else:
                os.kill(pid, signal.SIGTERM)
        wait('all owned GUI hosts released', lambda: not hosts())
        wait('terminal detached without ending work', lambda: all(not surface['attached'] for surface in workspace()['surfaces']))

    def process_identity():
        if windows:
            path = str(daemon_path).replace("'", "''")
            script = f"@(Get-CimInstance Win32_Process -Filter \"Name = 'compi-daemon.exe'\" | Where-Object {{$_.ExecutablePath -eq '{path}' -and $_.CommandLine -match '--instance {instance}(?:\\s|$)'}} | Select-Object ProcessId,CreationDate) | ConvertTo-Json -Compress"
            values = json.loads(run('powershell.exe', '-NoProfile', '-Command', script).stdout)
            values = values if isinstance(values, list) else [values]
            if len(values) != 1:
                raise RuntimeError('Expected one exactly-owned daemon process')
            return values[0]
        libproc = ctypes.CDLL('/usr/lib/libproc.dylib')
        libproc.proc_pidpath.argtypes = [ctypes.c_int, ctypes.c_void_p, ctypes.c_uint32]
        libproc.proc_pidpath.restype = ctypes.c_int
        pids = []
        for value in run('/bin/ps', '-axo', 'pid=').stdout.split():
            buffer = ctypes.create_string_buffer(4096)
            if libproc.proc_pidpath(int(value), buffer, len(buffer)) > 0 and os.fsdecode(buffer.value) == str(daemon_path):
                pids.append(int(value))
        if len(pids) != 1:
            raise RuntimeError('Expected one exactly-owned daemon process')
        return {'ProcessId': pids[0], 'CreationDate': run('/bin/ps', '-p', str(pids[0]), '-o', 'lstart=').stdout.strip()}

    def assert_daemon(original):
        pid = original['ProcessId']
        if windows:
            actual = json.loads(run('powershell.exe', '-NoProfile', '-Command', f'Get-CimInstance Win32_Process -Filter "ProcessId = {pid}" | Select-Object ProcessId,CreationDate | ConvertTo-Json -Compress').stdout)
            if actual != original:
                raise RuntimeError('Update replaced the daemon process identity')
        else:
            if run('/bin/ps', '-p', str(pid), '-o', 'lstart=').stdout.strip() != original['CreationDate']:
                raise RuntimeError('Update replaced the daemon process identity')

    def unchanged_install():
        if windows:
            if json.loads((root / 'selection.json').read_bytes()) != before_selection:
                raise RuntimeError('Failure/cancellation changed launch selection')
        elif plistlib.loads((root / 'Contents/Info.plist').read_bytes())['CFBundleShortVersionString'] != current:
            raise RuntimeError('Failure/cancellation replaced the prior bundle')
        for path, content in sentinels.items():
            if path.read_bytes() != content:
                raise RuntimeError(f'Operation changed preserved data: {path}')

    def stage(metadata=args.metadata, artifact=args.artifact, check=True):
        return run(worker, 'stage', '--root', root, '--metadata', metadata,
                   '--artifact', artifact, '--platform', platform, '--current-version', current, check=check)

    def inspect():
        value = json.loads(run(args.probe, '--instance', instance, 'surface', 'inspect', workspace()['surfaces'][0]['id']).stdout)
        def texts(node):
            if isinstance(node, dict):
                if 'text' in node and isinstance(node['text'], str):
                    yield node['text']
                for key, child in node.items():
                    if key != 'text':
                        yield from texts(child)
            elif isinstance(node, list):
                for child in node:
                    yield from texts(child)
        text = ''.join(texts(value))
        samples = re.findall(r'COMPI_CYCLE:(\d+):' + nonce + r':(/tmp/compi-cycle\.[^: ]+):(\d+)', text)
        if not samples:
            raise RuntimeError('Shell did not produce a cwd/environment/PID/counter sample')
        sample = max(samples, key=lambda row: int(row[2]))
        return {'shell_pid': sample[0], 'cwd': sample[1], 'counter': int(sample[2]), 'snapshot': value}

    succeeded = False
    try:
        initial = launch()
        daemon_identity = process_identity()
        close_gui()
        command = f'work=$(mktemp -d /tmp/compi-cycle.{nonce}.XXXXXX); cd "$work"; export COMPI_SMOKE_MARKER={shlex.quote(nonce)}; trap \'cd /tmp; rmdir "$work"\' EXIT; i=0; while :; do printf "COMPI_CYCLE:%s:%s:%s:%08d\\n" "$$" "$COMPI_SMOKE_MARKER" "$PWD" "$i"; i=$((i+1)); sleep 0.1; done'
        run(driver, 'seed-shell', instance, command)
        before_output = wait('active shell output', inspect)
        launch()
        original_identity = identity(initial)
        bad = dict(signed)
        signature = bytearray(base64.b64decode(bad['signature'], validate=True))
        signature[0] ^= 1
        bad['signature'] = base64.b64encode(signature).decode()
        bad_metadata = run_root / 'bad-signature.json'
        bad_metadata.write_text(json.dumps(bad))
        if stage(metadata=bad_metadata, check=False).returncode == 0:
            raise RuntimeError('Untrusted metadata was accepted')
        unchanged_install()
        broken = run_root / 'corrupted.zip'
        shutil.copyfile(args.artifact, broken)
        with broken.open('r+b') as stream:
            byte = stream.read(1)
            stream.seek(0)
            stream.write(bytes([byte[0] ^ 1]))
        if stage(artifact=broken, check=False).returncode == 0:
            raise RuntimeError('Corrupted full package was accepted')
        unchanged_install()
        run(driver, 'cancel-stage', args.metadata, args.artifact, current)
        unchanged_install()
        if identity(workspace()) != original_identity:
            raise RuntimeError('Trust rejection or cancellation ended active work')
        assert_daemon(daemon_identity)

        def apply(label, fail=False):
            handoff = run_root / f'{label}-handoff.json'
            rollback_handoff = run_root / f'{label}-rollback-handoff.json'
            host_file = run_root / f'{label}-hosts.json'
            result = stage()
            journal = Path(result.stdout.strip().splitlines()[-1])
            run(driver, 'prepare', instance, handoff, rollback_handoff, candidate['version'], host_file)
            current_hosts = json.loads(host_file.read_text())
            receipt = run_root / f'{label}-receipt.json'
            token = uuid.uuid4().hex + uuid.uuid4().hex
            rollback_receipt = run_root / f'{label}-rollback-receipt.json'
            rollback_token = uuid.uuid4().hex + uuid.uuid4().hex
            restore_path = run_root / 'deliberately-absent-handoff.json' if fail else handoff
            request = {'old_process_ids': [host['pid'] for host in current_hosts],
                       'hosts': [{'arguments': ['--update-restore', str(restore_path)],
                                  'receipt_path': str(receipt), 'receipt_token': token}],
                       'rollback_hosts': [{'executable': current_hosts[0]['executable'],
                                           'version': current_hosts[0]['product_version'],
                                           'host': {'arguments': ['--update-restore', str(rollback_handoff)],
                                                    'receipt_path': str(rollback_receipt),
                                                    'receipt_token': rollback_token}}],
                       # The smoke keeps a compatible live daemon, so the client would preserve it.
                       'replace_default_daemon': False,
                       # Covers old-GUI exit plus the deliberately failing restore; macOS GUI exit can exceed 10 s.
                       'timeout_seconds': 30 if fail else 45}
            request_path = run_root / f'{label}-request.json'
            request_path.write_text(json.dumps(request))
            with (run_root / f'{label}-worker.log').open('wb') as output:
                helper = subprocess.Popen([str(worker), 'apply', '--journal', str(journal), '--request', str(request_path)], env=environment, stdout=output, stderr=output)
                try:
                    run(driver, 'release', host_file, handoff)
                except Exception:
                    run(driver, 'abort', host_file, handoff, check=False)
                    raise
                code = helper.wait(timeout=90)
            if fail:
                if code == 0:
                    raise RuntimeError('Missing real restore handoff falsely reported activation success')
                run(worker, 'recover', '--root', root)
                unchanged_install()
                observed = json.loads(rollback_receipt.read_text())
                if observed.get('token') != rollback_token or observed.get('version') != current or not observed.get('attached') or observed.get('error'):
                    raise RuntimeError('Rollback did not prove actual old-build exact-slot attachment')
                if identity(wait('old B exact-slot attachment after rollback', attached)) != original_identity:
                    raise RuntimeError('Failed relaunch/rollback changed shell or workspace identity')
            else:
                if code != 0:
                    raise RuntimeError(f'Worker activation failed; inspect {run_root}')
                observed = json.loads(receipt.read_text())
                if observed.get('token') != token or observed.get('version') != candidate['version'] or not observed.get('attached') or observed.get('error'):
                    raise RuntimeError('No actual C GUI attachment readiness proof')
                if identity(wait('C attached to B daemon/shell', attached)) != original_identity:
                    raise RuntimeError('Compatible update replaced live daemon/shell/workspace identity')
            assert_daemon(daemon_identity)

        apply('failed-relaunch', fail=True)
        apply('compatible-update')
        # After success driver still belongs to B. Use the registry root scope but
        # restore the diagnostic into the current Mac bundle for final owned cleanup.
        if not windows:
            shutil.copy2(args.driver, driver)
        close_gui()
        after_output = wait('continued output from the original shell', inspect)
        if (after_output['shell_pid'] != before_output['shell_pid'] or after_output['cwd'] != before_output['cwd'] or after_output['counter'] <= before_output['counter']):
            raise RuntimeError('PID/cwd/environment/output continuity failed')
        for path, content in sentinels.items():
            if path.read_bytes() != content:
                raise RuntimeError('Successful activation changed user data')
        evidence = {'schema': 1, 'platform': platform, 'from_version': current, 'to_version': candidate['version'],
                    'artifact_sha256': digest(args.artifact), 'metadata_sha256': digest(args.metadata),
                    'daemon_identity': daemon_identity, 'workspace_identity': original_identity,
                    'from_artifact_sha256': owned.get('artifact_sha256'),
                    'before_output': before_output, 'after_output': after_output,
                    'observed': ['signature-rejection', 'digest-rejection', 'cancelled-stage',
                                 'failed-real-restore-rollback', 'journal-recovery', 'compatible-real-GUI-restore']}
        (run_root / 'evidence.json').write_text(json.dumps(evidence, indent=2) + '\n')
        succeeded = True
        print(f'PASS {current}->{candidate["version"]}; exact hashes and readiness evidence: {run_root / "evidence.json"}')
    finally:
        try:
            if not windows and not driver.exists():
                shutil.copy2(args.driver, driver)
            close_gui()
            run(args.probe, '--instance', instance, 'shutdown', check=False)
            for process in launched:
                process.wait(timeout=10)
        finally:
            log.close()
            if driver.exists():
                driver.unlink()
        if not succeeded:
            print(f'FAILED; owned root retained, diagnostics: {run_root}', file=sys.stderr)


if __name__ == '__main__':
    main()
