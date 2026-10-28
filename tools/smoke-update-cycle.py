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
    parser.add_argument('--installed-fixture', action='store_true',
                        help='Exercise real installed task replacement only on an owned GitHub-hosted Windows runner')
    args = parser.parse_args()
    if sys.platform not in ('win32', 'darwin'):
        parser.error('Run on native Windows x64/WSL2 or Apple Silicon macOS')
    root = args.root.resolve(strict=True)
    marker = root.parent / f'.{root.name}.compi-smoke-owned.json'
    owned = json.loads(marker.read_text())
    if owned.get('root') != str(root) or not owned.get('nonce'):
        parser.error('Root is not a script-owned disposable package copy')
    windows = sys.platform == 'win32'
    installed = args.installed_fixture
    if installed:
        if not windows or os.environ.get('GITHUB_ACTIONS') != 'true' or os.environ.get('RUNNER_ENVIRONMENT') != 'github-hosted':
            parser.error('Installed fixture requires native GitHub-hosted Windows')
        sid = subprocess.check_output(
            ['powershell.exe', '-NoProfile', '-Command',
             '[Security.Principal.WindowsIdentity]::GetCurrent().User.Value'], text=True).strip()
        expected = Path(os.environ['LOCALAPPDATA']) / 'Programs' / 'Compi'
        if root != expected.resolve() or owned.get('sid') != sid or owned.get('fixture') != 'installed-update':
            parser.error('Installed root/account is not owned by this qualification')
        if not (root / 'Compi-Setup.exe').is_file():
            parser.error('Installed fixture must retain its real Setup registration')
        if any(os.environ.get(name) for name in ('COMPI_DATA_DIR', 'COMPI_RUNTIME_DIR', 'COMPI_CONFIG_FILE', 'COMPI_INSTANCE')):
            parser.error('Installed fixture must use the actual disposable account environment')
        preflight = r'''
$ErrorActionPreference='Stop'
$root=Join-Path $env:LOCALAPPDATA 'Programs\Compi'
$sid=[Security.Principal.WindowsIdentity]::GetCurrent().User.Value
$r=Get-ItemProperty 'HKCU:\Software\Microsoft\Windows\CurrentVersion\Uninstall\Compi'
$s=Get-Content -LiteralPath (Join-Path $root 'selection.json') -Raw | ConvertFrom-Json
$t=Get-ScheduledTask -TaskName ('Compi Daemon-'+$sid) -TaskPath '\'
$owner=$t.Principal.UserId
if ($owner -notlike 'S-1-*') { $owner=([Security.Principal.NTAccount]$owner).Translate([Security.Principal.SecurityIdentifier]).Value }
$actions=@($t.Actions)
$expected=Join-Path $root ('versions\'+$s.task_version+'\compi-daemon.exe')
if ($owner -ne $sid -or [IO.Path]::GetFullPath($r.InstallLocation).TrimEnd('\') -ne $root -or
    $actions.Count -ne 1 -or $actions[0].Execute.Trim('"') -ne $expected -or
    $actions[0].Arguments -ne '--supervise') { throw 'Installed task/registration is not exactly owned' }
$default=@(Get-CimInstance Win32_Process -Filter "Name = 'compi-daemon.exe'" | Where-Object {
    $_.ExecutablePath -and $_.ExecutablePath.StartsWith($root+'\',[StringComparison]::OrdinalIgnoreCase) -and
    $_.CommandLine -notmatch '--instance(?:\s|=)'
})
if ($default.Count) { throw 'Installed cycle requires no running default daemon; never stop unreviewed work here' }
'''
        subprocess.run(['powershell.exe', '-NoProfile', '-NonInteractive', '-Command', preflight], check=True)
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
    if candidate['platform'] != platform or current not in candidate['qualified_daemon_versions'] or candidate['daemon_protocol'] != 35:
        parser.error('Compatible cycle requires signed native qualification of the running B daemon')
    if tuple(map(int, candidate['version'].split('.'))) <= tuple(map(int, current.split('.'))):
        parser.error('C must be genuinely newer than B')
    run_root = args.evidence.resolve() / ('cycle-' + uuid.uuid4().hex[:12])
    run_root.mkdir(parents=True)
    profile = run_root / 'profile'
    profile.mkdir(mode=0o700)
    environment = os.environ.copy()
    environment.pop('COMPI_SUPERVISOR_PID', None)
    if windows:
        if not installed:
            environment['LOCALAPPDATA'] = str(profile)
        data = Path(environment['LOCALAPPDATA']) / 'Compi'
    else:
        data = profile / 'data'
        # Unix socket paths are limited to ~104 bytes; keep the runtime dir short.
        runtime = Path(tempfile.mkdtemp(prefix='compi-cycle-', dir='/tmp')).resolve()
        environment.update(COMPI_DATA_DIR=str(data), COMPI_RUNTIME_DIR=str(runtime))
    data.mkdir(mode=0o700, exist_ok=installed)
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
    if installed:
        (run_root / 'ownership.json').write_text(json.dumps({
            'schema': 1, 'root': str(root), 'sid': sid, 'nonce': owned['nonce'],
            'instance': instance, 'daemon': str(daemon_path)}))
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

    task_observations = []

    def task_snapshot(label, version):
        if not installed:
            return None
        script = r'''
$ErrorActionPreference='Stop'
$sid=[Security.Principal.WindowsIdentity]::GetCurrent().User.Value
$r=Get-ItemProperty 'HKCU:\Software\Microsoft\Windows\CurrentVersion\Uninstall\Compi'
$t=Get-ScheduledTask -TaskName ('Compi Daemon-'+$sid) -TaskPath '\'
$owner=$t.Principal.UserId
if ($owner -notlike 'S-1-*') { $owner=([Security.Principal.NTAccount]$owner).Translate([Security.Principal.SecurityIdentifier]).Value }
@{sid=$sid;owner_sid=$owner;name=$t.TaskName;path=$t.TaskPath;actions=@($t.Actions | Select-Object Execute,Arguments);root=$r.InstallLocation;xml=(Export-ScheduledTask -TaskName $t.TaskName -TaskPath $t.TaskPath).Trim()} | ConvertTo-Json -Depth 5 -Compress
'''
        value = json.loads(run('powershell.exe', '-NoProfile', '-NonInteractive', '-Command', script).stdout)
        actions = value['actions']
        expected = root / 'versions' / version / 'compi-daemon.exe'
        if (value['sid'] != sid or value['owner_sid'] != sid or value['path'] != '\\'
                or len(actions) != 1 or actions[0]['Execute'].startswith('\\\\?\\')
                or Path(actions[0]['Execute'].strip('"')).resolve() != expected.resolve()
                or actions[0]['Arguments'] != '--supervise'
                or Path(value['root']).resolve() != root):
            raise RuntimeError('Installed registration/task escaped the owned account, root, or selected generation')
        task_observations.append({'label': label, 'selection': json.loads((root / 'selection.json').read_bytes()),
                                  'task': value})
        return value

    initial_task = task_snapshot('before-cycle', current)
    if installed and before_selection['task_version'] != current:
        raise RuntimeError('Fixture must start with the current selected task generation')

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

    def capture_windows(label):
        if not windows:
            return
        current_hosts = hosts()
        if len(current_hosts) != 1:
            raise RuntimeError('Native capture requires one exactly-owned GUI host')
        image = str(run_root / (label + '.png')).replace("'", "''")
        script = f'''
$ErrorActionPreference='Stop'
Add-Type -AssemblyName System.Drawing
Add-Type -TypeDefinition 'using System; using System.Runtime.InteropServices; public class UpdateSmokeWindow {{ [DllImport("user32.dll")] public static extern bool SetProcessDPIAware(); [DllImport("user32.dll")] public static extern bool SetForegroundWindow(IntPtr h); [DllImport("user32.dll")] public static extern bool GetWindowRect(IntPtr h, out Rect r); public struct Rect {{ public int Left, Top, Right, Bottom; }} }}'
[void][UpdateSmokeWindow]::SetProcessDPIAware()
$window=(Get-Process -Id {current_hosts[0]['pid']}).MainWindowHandle
if ($window -eq [IntPtr]::Zero) {{ throw 'Owned GUI has no native window' }}
[void][UpdateSmokeWindow]::SetForegroundWindow($window)
Start-Sleep -Milliseconds 500
$rect=New-Object UpdateSmokeWindow+Rect
if (-not [UpdateSmokeWindow]::GetWindowRect($window,[ref]$rect)) {{ throw 'Cannot inspect owned GUI bounds' }}
$bitmap=New-Object Drawing.Bitmap ($rect.Right-$rect.Left),($rect.Bottom-$rect.Top)
$graphics=[Drawing.Graphics]::FromImage($bitmap)
try {{ $graphics.CopyFromScreen($rect.Left,$rect.Top,0,0,$bitmap.Size); $bitmap.Save('{image}',[Drawing.Imaging.ImageFormat]::Png) }}
finally {{ $graphics.Dispose(); $bitmap.Dispose() }}
'''
        run('powershell.exe', '-NoProfile', '-NonInteractive', '-Command', script)

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
        result = run(worker, 'stage', '--root', root, '--metadata', metadata,
                     '--artifact', artifact, '--platform', platform, '--current-version', current, check=check)
        if installed and result.returncode == 0:
            journal = json.loads(Path(result.stdout.strip().splitlines()[-1]).read_text())
            if journal['target']['kind'] != 'InstalledWindows' or not journal['target']['root'].startswith('\\\\?\\'):
                raise RuntimeError('Installed staging did not use the actual canonical installed target')
        return result

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

        def failed_apply(label, preactivation=False):
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
            restore_path = handoff if preactivation else run_root / 'deliberately-absent-handoff.json'
            request = {'old_process_ids': [host['pid'] for host in current_hosts],
                       'hosts': [{'arguments': ['--update-restore', str(restore_path)],
                                  'receipt_path': str(receipt), 'receipt_token': token}],
                       'rollback_hosts': [{'executable': current_hosts[0]['executable'],
                                           'version': current_hosts[0]['product_version'],
                                           'host': {'arguments': ['--update-restore', str(rollback_handoff)],
                                                    'receipt_path': str(rollback_receipt),
                                                    'receipt_token': rollback_token}}],
                       # A named compatible shell remains alive, but no default daemon
                       # is running: the actual installed client must replace its task.
                       'replace_default_daemon': installed,
                       # Covers old-GUI exit plus the deliberately failing restore; macOS GUI exit can exceed 10 s.
                       'timeout_seconds': 30}
            request_path = run_root / f'{label}-request.json'
            request_path.write_text(json.dumps(request))
            if preactivation:
                retained = Path(json.loads(journal.read_text())['stage']).parent / 'package.zip'
                with retained.open('r+b') as stream:
                    byte = stream.read(1)
                    stream.seek(0)
                    stream.write(bytes([byte[0] ^ 1]))
            with (run_root / f'{label}-worker.log').open('wb') as output:
                observer = None
                if installed and not preactivation:
                    observed_path = run_root / f'{label}-new-task.json'
                    ready_path = run_root / f'{label}-observer-ready'
                    def ps_quote(value):
                        return "'" + str(value).replace("'", "''") + "'"
                    # Initialize a fast, persistent COM observer before activation:
                    # a failed GUI can roll back faster than spawning PowerShell.
                    observer_script = f'''
$ErrorActionPreference='Stop'
$service=New-Object -ComObject 'Schedule.Service'
$service.Connect()
$folder=$service.GetFolder('\\')
$sid=[Security.Principal.WindowsIdentity]::GetCurrent().User.Value
$name='Compi Daemon-'+$sid
$expected={ps_quote(root / 'versions' / candidate['version'] / 'compi-daemon.exe')}
[IO.File]::WriteAllText({ps_quote(ready_path)},'ready')
$deadline=[DateTime]::UtcNow.AddSeconds(60)
while ([DateTime]::UtcNow -lt $deadline) {{
    $t=$folder.GetTask($name)
    $action=$t.Definition.Actions.Item(1)
    if ($action.Path.Trim('"') -eq $expected) {{
        $owner=$t.Definition.Principal.UserId
        if ($owner -notlike 'S-1-*') {{ $owner=([Security.Principal.NTAccount]$owner).Translate([Security.Principal.SecurityIdentifier]).Value }}
        if ($owner -ne $sid -or $t.Definition.Actions.Count -ne 1 -or $action.Arguments -ne '--supervise') {{ throw 'Replacement task is not owned' }}
        $selection=Get-Content -LiteralPath {ps_quote(root / 'selection.json')} -Raw | ConvertFrom-Json
        $r=Get-ItemProperty 'HKCU:\\Software\\Microsoft\\Windows\\CurrentVersion\\Uninstall\\Compi'
        $value=@{{label='failed-relaunch-new-task';selection=$selection;task=@{{sid=$sid;owner_sid=$owner;name=$name;path='\\';root=$r.InstallLocation;actions=@(@{{Execute=$action.Path;Arguments=$action.Arguments}});xml=$t.Xml.Trim()}}}}
        [IO.File]::WriteAllText({ps_quote(observed_path)},($value | ConvertTo-Json -Depth 10),[Text.UTF8Encoding]::new($false))
        exit 0
    }}
    Start-Sleep -Milliseconds 10
}}
throw 'Never observed the real installed replacement task before rollback'
'''
                    observer = subprocess.Popen(
                        ['powershell.exe', '-NoProfile', '-NonInteractive', '-Command', observer_script],
                        env=environment, stdout=output, stderr=output)
                try:
                    if observer:
                        wait('installed task observer initialized', ready_path.exists, timeout=20)
                    helper = subprocess.Popen([str(worker), 'apply', '--journal', str(journal), '--request', str(request_path)], env=environment, stdout=output, stderr=output)
                    try:
                        run(driver, 'release', host_file, handoff)
                    except Exception:
                        run(driver, 'abort', host_file, handoff, check=False)
                        raise
                    if observer:
                        if observer.wait(timeout=65) != 0:
                            raise RuntimeError('Failed to observe actual new installed task before rollback')
                        task_observations.append(json.loads(observed_path.read_text()))
                    code = helper.wait(timeout=90)
                finally:
                    if observer and observer.poll() is None:
                        observer.kill()
                        observer.wait(timeout=10)
            if code == 0:
                raise RuntimeError('Injected update failure falsely reported activation success')
            failed_journal = json.loads(journal.read_text())
            if installed:
                (run_root / f'{label}-journal.json').write_text(json.dumps(failed_journal, indent=2) + '\n')
            if not failed_journal.get('error'):
                raise RuntimeError('Update failure was not persisted for the reopened GUI and CLI')
            run(worker, 'recover', '--root', root)
            if installed:
                restored_task = task_snapshot(label + '-old-task-restored', current)
                if restored_task['xml'] != initial_task['xml']:
                    raise RuntimeError('Failure did not restore the exact old task registration')
                if not preactivation and not failed_journal.get('prior_task'):
                    raise RuntimeError('Injected restore failure never captured the installed task')
            unchanged_install()
            observed = json.loads(rollback_receipt.read_text())
            if observed.get('token') != rollback_token or observed.get('version') != current or not observed.get('attached') or observed.get('error'):
                raise RuntimeError('Failure did not prove actual old-build exact-slot attachment')
            if identity(wait('old B exact-slot attachment after failure', attached)) != original_identity:
                raise RuntimeError('Failed update changed shell or workspace identity')
            capture_windows(label + '-reopened')
            assert_daemon(daemon_identity)

        failed_apply('failed-preactivation', preactivation=True)
        failed_apply('failed-relaunch')
        result = stage()
        journal = Path(result.stdout.strip().splitlines()[-1])
        outcome = root / '.compi-update' / ('cli-install-' + uuid.uuid4().hex + '.json')
        # Use the same reviewed GUI install entrypoint as Settings and `compi update`;
        # do not substitute a successful direct worker call for the in-app path.
        run(driver, 'install', instance, candidate['version'], outcome)

        def installed_and_reopened():
            status = json.loads(journal.read_text())
            if status.get('error'):
                raise RuntimeError('GUI update failed: ' + status['error'])
            if outcome.exists():
                reported = json.loads(outcome.read_text())
                if reported.get('outcome') == 'failed':
                    raise RuntimeError('GUI refused install: ' + reported['message'])
            return status if status['phase'] == 'Complete' else None

        completed = wait('reviewed GUI install and actual new-build attachment', installed_and_reopened, timeout=180)
        request = json.loads((journal.parent / ('request-' + completed['attempt'] + '.json')).read_text())
        if installed:
            if request['replace_default_daemon'] is not True:
                raise RuntimeError('Real GUI install did not request installed default task replacement')
            task_snapshot('successful-new-task', candidate['version'])
            selected = json.loads((root / 'selection.json').read_bytes())
            if selected['version'] != candidate['version'] or selected['task_version'] != candidate['version']:
                raise RuntimeError('Successful GUI install did not select both new installed generations')
        for host in request['hosts']:
            observed = json.loads(Path(host['receipt_path']).read_text())
            if observed.get('token') != host['receipt_token'] or observed.get('version') != candidate['version'] or not observed.get('attached') or observed.get('error'):
                raise RuntimeError('No actual C GUI attachment readiness proof')
        if identity(wait('C attached to B daemon/shell', attached)) != original_identity:
            raise RuntimeError('Compatible GUI update replaced daemon/shell/workspace identity')
        assert_daemon(daemon_identity)
        capture_windows('installed-and-reopened')
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
                                 'failed-preactivation-real-GUI-reopen', 'failed-real-restore-rollback',
                                 'journal-recovery', 'reviewed-in-app-install-and-real-GUI-restore']}
        if installed:
            evidence.update(installation_kind='InstalledWindows', fixture=True,
                            account_sid=sid, task_observations=task_observations,
                            stable_helper_sha256=digest(worker),
                            baseline_fixture_gui_sha256=digest(gui),
                            baseline_fixture_daemon_sha256=digest(daemon_path),
                            fixture_source_setup_sha256=owned.get('source_setup_sha256'))
            evidence['observed'].extend(['real-installed-task-replacement',
                                         'exact-old-task-rollback', 'new-task-same-account-and-root'])
        (run_root / 'evidence.json').write_text(json.dumps(evidence, indent=2) + '\n')
        succeeded = True
        print(f'PASS {current}->{candidate["version"]}; exact hashes and readiness evidence: {run_root / "evidence.json"}')
    finally:
        try:
            if not windows and not driver.exists():
                shutil.copy2(args.driver, driver)
            close_gui()
            run(args.probe, '--existing', '--instance', instance, 'shutdown', check=installed)
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
