#!/usr/bin/env python3
"""Sign only full payloads whose identities were exercised by both native jobs."""
import argparse
import hashlib
import json
from pathlib import Path
import re
import subprocess


def main():
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument('--distribution', type=Path, required=True)
    parser.add_argument('--tag', required=True)
    parser.add_argument('--repository', required=True)
    parser.add_argument('--signer', type=Path, required=True)
    parser.add_argument('--notes', type=Path, required=True)
    args = parser.parse_args()
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
                or qualification.get('daemon_protocol') != 18):
            raise SystemExit(f'Native qualification does not match {filename}')
        daemons = qualification.get('qualified_daemons')
        if not isinstance(daemons, list) or not daemons or any(
                not isinstance(item, str) or not re.fullmatch(r'\d+\.\d+\.\d+', item) for item in daemons):
            raise SystemExit(f'Missing exact native-qualified daemon versions for {platform}')
        verified.append((platform, artifact, minimum_os, daemons))
    for platform, artifact, minimum_os, daemons in verified:
        command = [str(args.signer.resolve()), 'sign', '--version', version,
                   '--platform', platform, '--artifact', str(artifact.resolve()),
                   '--url', f'https://github.com/{args.repository}/releases/download/{args.tag}/{artifact.name}',
                   '--output', str(args.distribution.resolve()), '--daemon-protocol', '18',
                   '--minimum-os', minimum_os, '--notes', str(args.notes.resolve())]
        command.extend(['--qualification', str((args.distribution / f'qualification-{platform}.json').resolve())])
        for daemon in sorted(set(daemons)):
            command.extend(['--qualified-daemon', daemon])
        if platform == 'windows-x86_64':
            # Signed separately into compi-setup-<platform>.json; repair downloads this Setup.
            setup = args.distribution / f'Compi-{version}-Setup.exe'
            if not setup.is_file():
                raise SystemExit(f'Missing Windows Setup {setup.name}')
            command.extend(['--setup', str(setup.resolve()), '--setup-url',
                            f'https://github.com/{args.repository}/releases/download/{args.tag}/{setup.name}'])
        subprocess.run(command, check=True)


if __name__ == '__main__':
    main()
