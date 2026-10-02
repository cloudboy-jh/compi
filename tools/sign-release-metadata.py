#!/usr/bin/env python3
"""Sign only full payloads whose identities were exercised by both native jobs."""
import argparse
import hashlib
import json
from pathlib import Path
import re
import subprocess

# Each native job must prove these on the exact source commit being released.
INSTALLER_SCENARIOS = {
    'windows-x86_64': {'install', 'cancel-repair', 'repair', 'uninstall',
                       'reinstall-preserved-data', 'remove-reinstalled', 'legacy-migration'},
    'macos-aarch64': {'dmg-copy-launch-reconnect', 'legacy-bundle-replacement'},
}
UPDATE_CYCLE_OBSERVED = {'signature-rejection', 'digest-rejection', 'cancelled-stage',
                         'failed-real-restore-rollback', 'journal-recovery',
                         'compatible-real-GUI-restore'}


def version_tuple(value):
    if not isinstance(value, str) or not re.fullmatch(r'\d+\.\d+\.\d+', value):
        return None
    return tuple(map(int, value.split('.')))


def require_scenarios(platform, version, commit, qualification):
    if qualification.get('source_commit') != commit:
        raise SystemExit(f'{platform} qualification was not produced from commit {commit}')
    scenarios = qualification.get('installer_scenarios')
    missing = INSTALLER_SCENARIOS[platform] - set(scenarios if isinstance(scenarios, list) else [])
    if missing:
        raise SystemExit(f'{platform} qualification lacks installer scenarios: {sorted(missing)}')
    cycle = qualification.get('update_cycle')
    if not isinstance(cycle, dict):
        raise SystemExit(f'{platform} qualification lacks update-cycle evidence')
    target = version_tuple(cycle.get('to_version'))
    if (cycle.get('schema') != 1 or cycle.get('platform') != platform
            or cycle.get('source_commit') != commit or cycle.get('from_version') != version
            or target is None or target <= version_tuple(version)):
        raise SystemExit(f'{platform} update-cycle evidence does not start from {version} at {commit}')
    missing = UPDATE_CYCLE_OBSERVED - set(cycle.get('observed') or [])
    if missing:
        raise SystemExit(f'{platform} update cycle did not observe: {sorted(missing)}')


def main():
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument('--distribution', type=Path, required=True)
    parser.add_argument('--tag', required=True)
    parser.add_argument('--repository', required=True)
    parser.add_argument('--signer', type=Path, required=True)
    parser.add_argument('--notes', type=Path, required=True)
    parser.add_argument('--commit', required=True)
    args = parser.parse_args()
    if not re.fullmatch(r'[0-9a-f]{40}', args.commit):
        parser.error('Commit must be a full lowercase SHA-1')
    if not re.fullmatch(r'v\d+\.\d+\.\d+', args.tag):
        parser.error('Stable metadata requires an exact vMAJOR.MINOR.PATCH tag')
    if not re.fullmatch(r'[A-Za-z0-9_.-]+/[A-Za-z0-9_.-]+', args.repository):
        parser.error('Invalid GitHub repository')
    version = args.tag[1:]
    platforms = [('windows-x86_64', f'Compi-{version}-Windows-x64-update.zip', '10.0.19041'),
                 ('macos-aarch64', f'Compi-{version}-macOS-arm64.app.zip', '14.0')]
    verified = []
    # Validate both native receipts before writing either public metadata file.
    for platform, filename, minimum_os in platforms:
        artifact = args.distribution / filename
        qualification = json.loads((args.distribution / f'qualification-{platform}.json').read_text())
        with artifact.open('rb') as stream:
            digest = hashlib.file_digest(stream, 'sha256').hexdigest()
        if (qualification.get('schema') != 1 or qualification.get('platform') != platform
                or qualification.get('version') != version
                or qualification.get('artifact_sha256') != digest
                or qualification.get('daemon_protocol') != 14):
            raise SystemExit(f'Native qualification does not match {filename}')
        daemons = qualification.get('qualified_daemons')
        if not isinstance(daemons, list) or not daemons or any(
                not isinstance(item, str) or not re.fullmatch(r'\d+\.\d+\.\d+', item) for item in daemons):
            raise SystemExit(f'Missing exact native-qualified daemon versions for {platform}')
        require_scenarios(platform, version, args.commit, qualification)
        verified.append((platform, artifact, minimum_os, daemons))
    for platform, artifact, minimum_os, daemons in verified:
        command = [str(args.signer.resolve()), 'sign', '--version', version,
                   '--platform', platform, '--artifact', str(artifact.resolve()),
                   '--url', f'https://github.com/{args.repository}/releases/download/{args.tag}/{artifact.name}',
                   '--output', str(args.distribution.resolve()), '--daemon-protocol', '14',
                   '--minimum-os', minimum_os, '--notes', str(args.notes.resolve())]
        command.extend(['--qualification', str((args.distribution / f'qualification-{platform}.json').resolve())])
        for daemon in sorted(set(daemons)):
            command.extend(['--qualified-daemon', daemon])
        subprocess.run(command, check=True)


if __name__ == '__main__':
    main()
