#!/usr/bin/env python3
"""Create a new exFAT data disk with /BIN, without mounts or root privileges.

Uses mkfs.exfat for formatting and fsck.exfat to verify the result. Only
fresh images are populated; existing destinations are never overwritten.
"""
import argparse
import os
from pathlib import Path
import struct
import subprocess
import tempfile

ROOT = Path(__file__).resolve().parent.parent


def rotate16(value):
    return ((value >> 1) | (value << 15)) & 0xffff


def entry(name, cluster, size, directory=False):
    """Build an ASCII exFAT file/stream/name entry set."""
    encoded = name.encode('utf-16le')
    entries = bytearray(64 + 32 * ((len(name) + 14) // 15))
    entries[0:2] = bytes([0x85, len(entries) // 32 - 1])
    struct.pack_into('<H', entries, 4, 0x10 if directory else 0x20)
    entries[32:34] = bytes([0xc0, 1 if directory else 3])
    entries[35] = len(name)
    hashed = 0
    for byte in name.upper().encode('utf-16le'):
        hashed = (rotate16(hashed) + byte) & 0xffff
    struct.pack_into('<H', entries, 36, hashed)
    struct.pack_into('<Q', entries, 40, size)
    struct.pack_into('<I', entries, 52, cluster)
    struct.pack_into('<Q', entries, 56, size)
    for index in range((len(name) + 14) // 15):
        offset = 64 + index * 32
        entries[offset] = 0xc1
        chunk = encoded[index * 30:(index + 1) * 30]
        entries[offset + 2:offset + 2 + len(chunk)] = chunk
    checksum = 0
    for index, byte in enumerate(entries):
        if index not in (2, 3):
            checksum = (rotate16(checksum) + byte) & 0xffff
    struct.pack_into('<H', entries, 2, checksum)
    return entries


def populate(disk, programs):
    data = bytearray(disk.read_bytes())
    sector = 1 << data[108]
    cluster_bytes = sector << data[109]
    fat = struct.unpack_from('<I', data, 80)[0] * sector
    heap = struct.unpack_from('<I', data, 88)[0] * sector
    count = struct.unpack_from('<I', data, 92)[0]
    root = struct.unpack_from('<I', data, 96)[0]

    def offset(cluster):
        return heap + (cluster - 2) * cluster_bytes

    root_offset = offset(root)
    bitmap_offset = None
    free_entry = None
    for pos in range(root_offset, root_offset + cluster_bytes, 32):
        if data[pos] == 0x81:
            bitmap_offset = offset(struct.unpack_from('<I', data, pos + 20)[0])
        if data[pos] == 0:
            free_entry = pos
            break
    if bitmap_offset is None or free_entry is None:
        raise ValueError('unsupported freshly formatted exFAT layout')

    def allocate(number):
        run = 0
        for index in range(count):
            if data[bitmap_offset + index // 8] & (1 << (index % 8)):
                run = 0
            else:
                run += 1
            if run == number:
                first = index - number + 1
                for bit in range(first, index + 1):
                    data[bitmap_offset + bit // 8] |= 1 << (bit % 8)
                return first + 2
        raise ValueError('disk image full')

    directory = allocate(1)
    struct.pack_into('<I', data, fat + directory * 4, 0xffffffff)
    cursor = offset(directory)
    for name, binary in programs.items():
        if not name or not name.isascii() or len(name) > 255 or any(c in name for c in '/\\:'):
            raise ValueError('invalid program name')
        first = allocate((len(binary) + cluster_bytes - 1) // cluster_bytes) if binary else 0
        if binary:
            data[offset(first):offset(first) + len(binary)] = binary
        file_set = entry(name, first, len(binary))
        if cursor + len(file_set) + 32 > offset(directory) + cluster_bytes:
            raise ValueError('/BIN entries exceed one cluster')
        data[cursor:cursor + len(file_set)] = file_set
        cursor += len(file_set)
    dir_set = entry('BIN', directory, cluster_bytes, directory=True)
    data[free_entry:free_entry + len(dir_set)] = dir_set
    # PercentInUse is optional and excluded from the boot checksum.
    data[112] = data[12 * sector + 112] = 0xff
    disk.write_bytes(data)


def create_image(disk, programs):
    disk = Path(disk)
    if os.path.lexists(disk):
        raise FileExistsError(f'{disk} already exists; refusing to overwrite it')
    # Build and verify next to the destination. Publish with link(), which
    # fails if another process creates the destination in the meantime.
    fd, temporary = tempfile.mkstemp(prefix='.mmurtl-disk-', suffix='.img', dir=disk.parent)
    temporary = Path(temporary)
    try:
        with os.fdopen(fd, 'wb') as stream:
            stream.truncate(16 * 1024 * 1024)
        subprocess.run(['mkfs.exfat', '-L', 'MMURTL', str(temporary)], check=True, stdout=subprocess.DEVNULL)
        populate(temporary, programs)
        subprocess.run(['fsck.exfat', '-n', str(temporary)], check=True, stdout=subprocess.DEVNULL)
        os.link(temporary, disk)
    finally:
        temporary.unlink(missing_ok=True)


if __name__ == '__main__':
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument('image', type=Path)
    parser.add_argument('target', choices=['x86_64-unknown-none', 'aarch64-unknown-none-softfloat'])
    args = parser.parse_args()
    try:
        binaries = ROOT / 'user' / 'target' / args.target / 'release'
        programs = {name.upper() + '.ELF': (binaries / name).read_bytes() for name in ['hello', 'sieve']}
        create_image(args.image, programs)
    except (OSError, ValueError, subprocess.CalledProcessError) as error:
        parser.exit(1, f'make-disk: {error}\n')
    for name in programs:
        print(f'  /BIN/{name}')
    print(f'Created {args.image}')
