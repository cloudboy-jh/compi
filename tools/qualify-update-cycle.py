#!/usr/bin/env python3
"""Prove an in-app B->C update on this native runner and attach it to the release receipt.

No earlier published release can update in-app yet, so B (this commit) and C (this
commit relabelled one patch version higher) are rebuilt with a disposable signing key
created here. The official artifacts and key are never used for the fixture; the
evidence is bound to the release by source commit.
"""
import argparse
import json
import os
from pathlib import Path
import re
import subprocess
import sys

ROOT = Path(__file__).resolve().parent.parent
WINDOWS = sys.platform == 'win32'
PLATFORM = 'windows-x86_64' if WINDOWS else 'macos-aarch64'
WORKSPACE_PACKAGES = ('compi-client', 'compi-daemon', 'compi-protocol', 'compi-update', 'compi-setup')
VERSIONED = ('Cargo.toml', 'Cargo.lock', 'installer/bootstrapper/Cargo.toml',
             'installer/bootstrapper/Cargo.lock')


def run(*command, env=None):
    print('+', ' '.join(str(part) for part in command), flush=True)
    return subprocess.run([str(part) for part in command], check=True, env=env, cwd=ROOT,
                          stdout=subprocess.PIPE, text=True).stdout


def workspace_version():
    manifest = (ROOT / 'Cargo.toml').read_text()
    match = re.search(r'(?ms)^\[workspace\.package\](?:(?!^\[).)*?^version\s*=\s*"(\d+)\.(\d+)\.(\d+)"', manifest)
    if not match:
        raise SystemExit('Cannot read workspace version')
    return tuple(map(int, match.groups()))


def relabel(current, candidate):
    """Rewrite only workspace-owned package versions; returns originals for restoration."""
    originals = {name: (ROOT / name).read_bytes() for name in VERSIONED}
    for name in VERSIONED:
        text = originals[name].decode()
        if name.endswith('Cargo.toml'):
            text, count = re.subn(r'(?m)^version = "' + re.escape(current) + '"', f'version = "{candidate}"', text, count=1)
        else:
            pattern = r'(name = "(?:' + '|'.join(WORKSPACE_PACKAGES) + r')"\r?\nversion = )"' + re.escape(current) + '"'
            text, count = re.subn(pattern, rf'\1"{candidate}"', text)
        if not count:
            raise SystemExit(f'No {current} workspace version found in {name}')
        (ROOT / name).write_bytes(text.encode())
    return originals


def build(output, env):
    if WINDOWS:
        run('pwsh', '-NoProfile', '-File', ROOT / 'tools/build-installer.ps1', '-OutputDirectory', output, env=env)
    else:
        run('bash', ROOT / 'tools/build-macos.sh', '--output', output, env=env)


def main():
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument('--qualification', type=Path, required=True,
                        help='official qualification receipt to extend')
    parser.add_argument('--signer', type=Path, required=True,
                        help='compi-release-metadata built by the official job')
    parser.add_argument('--commit', required=True)
    parser.add_argument('--repository', required=True)
    parser.add_argument('--work', type=Path, default=ROOT / 'target/update-qualification')
    args = parser.parse_args()
    work = args.work.resolve()
    if work.exists():
        raise SystemExit(f'Refusing to reuse qualification directory {work}')
    work.mkdir(parents=True)
    official = json.loads(args.qualification.read_text())
    major, minor, patch = workspace_version()
    current, candidate = f'{major}.{minor}.{patch}', f'{major}.{minor}.{patch + 1}'
    if official.get('platform') != PLATFORM or official.get('version') != current:
        raise SystemExit('Official qualification does not describe this platform/version')

    key = work / 'disposable-update.key'
    public = run(args.signer.resolve(), 'keygen', '--private-key', key).strip()
    env = dict(os.environ, COMPI_UPDATE_PUBLIC_KEY=public)
    # Fixture builds never use publisher identities or a legacy migration input.
    for name in ('COMPI_SMOKE_LEGACY_APP_ZIP', 'COMPI_SIGNING_THUMBPRINT', 'COMPI_MACOS_SIGNING_IDENTITY',
                 'COMPI_MACOS_NOTARY_PROFILE', 'COMPI_MACOS_NOTARY_KEYCHAIN'):
        env.pop(name, None)
    b_dir, c_dir, metadata_dir = work / 'B', work / 'C', work / 'C-metadata'
    build(b_dir, env)
    originals = relabel(current, candidate)
    try:
        build(c_dir, env)
    finally:
        for name, content in originals.items():
            (ROOT / name).write_bytes(content)

    artifact = c_dir / (f'Compi-{candidate}-Windows-x64-update.zip' if WINDOWS else f'Compi-{candidate}-macOS-arm64.app.zip')
    run(args.signer.resolve(), 'sign', '--key-file', key, '--version', candidate, '--platform', PLATFORM,
        '--artifact', artifact,
        '--url', f'https://github.com/{args.repository}/releases/download/v{candidate}/{artifact.name}',
        '--output', metadata_dir, '--daemon-protocol', '14', '--qualified-daemon', current,
        '--minimum-os', '10.0.19041' if WINDOWS else '14.0', env=env)
    metadata = metadata_dir / f'compi-update-{PLATFORM}.json'
    if WINDOWS:
        run('pwsh', '-NoProfile', '-File', ROOT / 'tools/smoke-windows-distribution.ps1',
            '-DistributionDirectory', b_dir, '-UpdateArtifact', artifact, '-UpdateMetadata', metadata, env=env)
    else:
        target_dir = ROOT / 'target/macos-release/product'
        run('cargo', 'build', '--locked', '--release', '--target', 'aarch64-apple-darwin',
            '--target-dir', target_dir, '-p', 'compi-client',
            '--example', 'compi-probe', '--example', 'compi-update-smoke',
            env=dict(env, MACOSX_DEPLOYMENT_TARGET='14.0'))
        run('bash', ROOT / 'tools/smoke-macos-bundle.sh', b_dir / f'Compi-{current}-macOS-arm64.dmg',
            target_dir / 'aarch64-apple-darwin/release/examples/compi-probe', artifact, metadata, env=env)

    cycle = json.loads((b_dir / f'qualification-{PLATFORM}.json').read_text()).get('update_cycle')
    if (not isinstance(cycle, dict) or cycle.get('platform') != PLATFORM
            or cycle.get('from_version') != current or cycle.get('to_version') != candidate):
        raise SystemExit('Update smoke did not record the expected B->C evidence')
    cycle.update(source_commit=args.commit, signing='disposable-qualification-key')
    official.update(source_commit=args.commit, update_cycle=cycle)
    args.qualification.write_text(json.dumps(official, indent=2) + '\n')
    key.unlink()
    print(f'PASS {PLATFORM} {current}->{candidate} update cycle recorded in {args.qualification}')


if __name__ == '__main__':
    main()
