#!/usr/bin/env python3
"""Extract the MSI embedded in the published v0.1.2 Setup.exe (legacy baseline A).

v0.1.2 published only its setup wrapper, which has no unattended mode. Its MSI is
an include_bytes! payload; both the wrapper and the extracted database are pinned.
"""
import argparse
import hashlib
from pathlib import Path
import struct

SETUP_SHA256 = 'c66ecb6733a2e6f9d9d1985b12c3c6c4d56385b00525f2676f26b4c833c7d717'
MSI_SHA256 = 'e58341c0325f2209b60aa720246f184229a36c2b0a56319fc617aa247bd35c42'
SIGNATURE = bytes.fromhex('D0CF11E0A1B11AE1')
FREE = 0xFFFFFFFF


def compound_file_length(data):
    """Length of a compound file from its FAT: the highest allocated sector bounds it."""
    sector = 1 << struct.unpack_from('<H', data, 0x1E)[0]
    entries = sector // 4
    fat_count = struct.unpack_from('<I', data, 0x2C)[0]
    next_difat = struct.unpack_from('<I', data, 0x44)[0]
    fat = list(struct.unpack_from('<109I', data, 0x4C))[:fat_count]
    while len(fat) < fat_count and next_difat < 0xFFFFFFFA:
        block = struct.unpack_from(f'<{entries}I', data, sector * (next_difat + 1))
        fat += block[:-1][:fat_count - len(fat)]
        next_difat = block[-1]
    highest = -1
    for index, fat_sector in enumerate(fat):
        for offset, value in enumerate(struct.unpack_from(f'<{entries}I', data, sector * (fat_sector + 1))):
            if value != FREE:
                highest = max(highest, index * entries + offset)
    # The header occupies one sector before sector 0.
    return sector * (highest + 2)


def main():
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument('setup', type=Path)
    parser.add_argument('output', type=Path)
    args = parser.parse_args()
    data = args.setup.read_bytes()
    if hashlib.sha256(data).hexdigest() != SETUP_SHA256:
        raise SystemExit('Setup.exe is not the published v0.1.2 wrapper')
    start = data.find(SIGNATURE)
    if start < 0 or data.find(SIGNATURE, start + 1) >= 0:
        raise SystemExit('Expected exactly one embedded MSI')
    msi = data[start:start + compound_file_length(data[start:])]
    if hashlib.sha256(msi).hexdigest() != MSI_SHA256:
        raise SystemExit('Extracted MSI does not match the pinned v0.1.2 database')
    args.output.write_bytes(msi)
    print(args.output)


if __name__ == '__main__':
    main()
