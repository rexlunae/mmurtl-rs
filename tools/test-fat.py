#!/usr/bin/env python3
"""Test the kernel FAT driver on fresh FAT12/16/32, MBR, and GPT images.

Requires the pinned Rust toolchain and dosfstools (mkfs.fat, fsck.fat).
Creates only temporary images; does not mount filesystems or require root.
"""
from pathlib import Path
import shutil
import struct
import subprocess
import tempfile
import uuid
import zlib

ROOT = Path(__file__).resolve().parent.parent


def formatted(path, bits):
    size = {12: 1440 * 1024, 16: 16 * 1024 * 1024, 32: 64 * 1024 * 1024}[bits]
    with path.open('wb') as stream:
        stream.truncate(size)
    subprocess.run(['mkfs.fat', '-F', str(bits), str(path)], check=True, stdout=subprocess.DEVNULL)


def partitioned(volume, target, gpt=False):
    payload = volume.read_bytes()
    start = 2048
    length = len(payload) // 512
    capacity = start + length + 2048
    image = bytearray(capacity * 512)
    image[start * 512:(start + length) * 512] = payload
    image[510:512] = b'\x55\xaa'
    image[450] = 0xee if gpt else 0x0e
    struct.pack_into('<II', image, 454, 1 if gpt else start, capacity - 1 if gpt else length)
    if gpt:
        table = bytearray(128 * 128)
        table[:16] = uuid.UUID('c12a7328-f81f-11d2-ba4b-00a0c93ec93b').bytes_le
        table[16:32] = uuid.UUID('01234567-89ab-cdef-0123-456789abcdef').bytes_le
        struct.pack_into('<QQ', table, 32, start, start + length - 1)
        image[1024:1024 + len(table)] = table
        image[(capacity - 33) * 512:(capacity - 1) * 512] = table

        def header(current, backup, entries):
            result = bytearray(512)
            result[:8] = b'EFI PART'
            struct.pack_into('<II', result, 8, 0x10000, 92)
            struct.pack_into('<QQQQ', result, 24, current, backup, 34, capacity - 34)
            result[56:72] = uuid.UUID('abcdef01-2345-6789-abcd-ef0123456789').bytes_le
            struct.pack_into('<QIII', result, 72, entries, 128, 128, zlib.crc32(table))
            struct.pack_into('<I', result, 16, zlib.crc32(result[:92]))
            return result

        image[512:1024] = header(1, capacity - 1, 2)
        image[-512:] = header(capacity - 1, 1, capacity - 33)
    target.write_bytes(image)
    return start, length


def main():
    for tool in ['rustc', 'mkfs.fat', 'fsck.fat']:
        if not shutil.which(tool):
            raise SystemExit(f'{tool} is required')
    with tempfile.TemporaryDirectory(prefix='mmurtl-fat-tests-') as directory:
        work = Path(directory)
        harness = work / 'fat-driver'
        subprocess.run(['rustc', '--edition=2021', str(ROOT / 'tools/tests/fat_driver.rs'), '-o', str(harness)], check=True)
        unit = work / 'fat-unit-tests'
        subprocess.run(['rustc', '--edition=2021', '--test', str(ROOT / 'tools/tests/fat_driver.rs'), '-o', str(unit)], check=True)
        subprocess.run([str(unit)], check=True)
        for bits in [12, 16, 32]:
            disk = work / f'fat{bits}.img'
            formatted(disk, bits)
            subprocess.run([str(harness), str(disk)], check=True)
            subprocess.run(['fsck.fat', '-n', str(disk)], check=True)
        for gpt in [False, True]:
            volume = work / 'partition.img'
            formatted(volume, 32 if gpt else 16)
            disk = work / ('gpt.img' if gpt else 'mbr.img')
            start, length = partitioned(volume, disk, gpt)
            subprocess.run([str(harness), str(disk)], check=True)
            with disk.open('rb') as stream:
                stream.seek(start * 512)
                volume.write_bytes(stream.read(length * 512))
            subprocess.run(['fsck.fat', '-n', str(volume)], check=True)
        for mode in ['--failure', '--root-full']:
            disk = work / 'failure-test.img'
            formatted(disk, 12)
            subprocess.run([str(harness), str(disk), mode], check=True)
            subprocess.run(['fsck.fat', '-n', str(disk)], check=True, stdout=subprocess.DEVNULL)
        # FAT32 with mirroring disabled must read and write only its active FAT.
        disk = work / 'active-fat.img'
        formatted(disk, 32)
        raw = bytearray(disk.read_bytes())
        reserved = struct.unpack_from('<H', raw, 14)[0]
        table_sectors = struct.unpack_from('<I', raw, 36)[0]
        struct.pack_into('<H', raw, 40, 0x81)
        backup = struct.unpack_from('<H', raw, 50)[0]
        struct.pack_into('<H', raw, backup * 512 + 40, 0x81)
        inactive = bytes(raw[reserved * 512:(reserved + table_sectors) * 512])
        # Reserved high bits must survive updates of a data-cluster entry.
        active_start = (reserved + table_sectors) * 512
        struct.pack_into('<I', raw, active_start + 10 * 4, 0xa0000000)
        disk.write_bytes(raw)
        subprocess.run([str(harness), str(disk)], check=True)
        after = disk.read_bytes()
        assert after[reserved * 512:(reserved + table_sectors) * 512] == inactive
        assert struct.unpack_from('<I', after, active_start + 10 * 4)[0] & 0xf0000000 == 0xa0000000
        # Reject a damaged GPT rather than interpreting arbitrary sectors.
        disk = work / 'bad-gpt.img'
        formatted(volume, 32)
        partitioned(volume, disk, True)
        raw = bytearray(disk.read_bytes())
        raw[512 + 16] ^= 1
        disk.write_bytes(raw)
        subprocess.run([str(harness), str(disk), '--reject'], check=True)
        partitioned(volume, disk, True)
        raw = bytearray(disk.read_bytes())
        raw[1024 + 56] ^= 1
        disk.write_bytes(raw)
        subprocess.run([str(harness), str(disk), '--reject'], check=True)
        print('Passed: FAT12/16/32, long names, Unicode, packed entries, fragmentation, directory growth, MBR, and GPT')


if __name__ == '__main__':
    main()
