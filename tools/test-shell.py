#!/usr/bin/env python3
"""Boot-test the CLI with a disposable exFAT disk; never touches user disks.

Build first: make bios user (amd64), or make arm64 user-arm64 (arm64).
Requires QEMU, mkfs.exfat, and fsck.exfat. Uses software emulation.
"""
import argparse
import json
import os
from pathlib import Path
import select
import shutil
import socket
import struct
import subprocess
import tempfile
import time

ROOT = Path(__file__).resolve().parent.parent


def rotate16(value):
    return ((value >> 1) | (value << 15)) & 0xffff


def entry(name, cluster, size, directory=False):
    """A minimal exFAT entry set for a deterministic test fixture."""
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


def make_fixture(disk, program):
    with disk.open('wb') as stream:
        stream.truncate(16 * 1024 * 1024)
    subprocess.run(['mkfs.exfat', str(disk)], check=True, stdout=subprocess.DEVNULL)
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
    assert bitmap_offset is not None and free_entry is not None

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
        raise AssertionError('fixture disk full')

    directory = allocate(1)
    binary = program.read_bytes()
    first = allocate((len(binary) + cluster_bytes - 1) // cluster_bytes)
    data[offset(first):offset(first) + len(binary)] = binary
    struct.pack_into('<I', data, fat + directory * 4, 0xffffffff)
    file_set = entry('HELLO.ELF', first, len(binary))
    data[offset(directory):offset(directory) + len(file_set)] = file_set
    dir_set = entry('BIN', directory, cluster_bytes, directory=True)
    data[free_entry:free_entry + len(dir_set)] = dir_set
    # PercentInUse is optional and excluded from the boot checksum.
    data[112] = data[12 * sector + 112] = 0xff
    disk.write_bytes(data)
    subprocess.run(['fsck.exfat', '-n', str(disk)], check=True, stdout=subprocess.DEVNULL)


def smoke(arch, cpus):
    target = 'x86_64-unknown-none' if arch == 'amd64' else 'aarch64-unknown-none-softfloat'
    program = ROOT / 'user' / 'target' / target / 'release' / 'hello'
    if arch == 'amd64':
        boot = ROOT / 'target/mmurtl-rs-bios.img'
        args = ['qemu-system-x86_64', '-drive', f'format=raw,file={boot}',
                '-display', 'none', '-serial', 'stdio', '-monitor', 'none']
    else:
        boot = ROOT / 'target' / target / 'release/mmurtl-rs'
        args = ['qemu-system-aarch64', '-machine', 'virt,gic-version=3',
                '-cpu', 'cortex-a72', '-kernel', str(boot), '-nographic', '-monitor', 'none']
    assert boot.is_file() and program.is_file(), 'build kernel/image and user programs first'
    for tool in [args[0], 'mkfs.exfat', 'fsck.exfat']:
        assert shutil.which(tool), f'{tool} is required'
    with tempfile.TemporaryDirectory(prefix='mmurtl-shell-') as directory:
        disk = Path(directory) / 'disk.img'
        make_fixture(disk, program)
        if arch == 'amd64':
            args += ['-drive', f'format=raw,file={disk},if=virtio']
        else:
            args += ['-drive', f'if=none,format=raw,file={disk},id=hd0',
                     '-device', 'virtio-blk-device,drive=hd0']
        qmp_path = Path(directory) / 'qmp.sock'
        if arch == 'amd64':
            args += ['-qmp', f'unix:{qmp_path},server=on,wait=off']
        proc = subprocess.Popen(args + ['-m', '256M', '-smp', str(cpus)],
                                stdin=subprocess.PIPE, stdout=subprocess.PIPE, stderr=subprocess.STDOUT)
        log = bytearray()

        def receive(needle, timeout=45):
            start = len(log)
            end = time.monotonic() + timeout
            while time.monotonic() < end:
                if select.select([proc.stdout], [], [], 0.1)[0]:
                    chunk = os.read(proc.stdout.fileno(), 65536)
                    if not chunk:
                        break
                    log.extend(chunk)
                    if needle in log[start:]:
                        return bytes(log[start:])
            raise AssertionError(f'timed out waiting for {needle!r}:\n{log[-4000:].decode(errors="replace")}')

        def command(text, expected=b'', prompt=b'/> '):
            proc.stdin.write(text + b'\r')
            proc.stdin.flush()
            reply = receive(prompt, 15)
            assert expected in reply, (text, expected, reply)
            return reply

        try:
            boot_log = receive(b'/> ')
            assert b'All userspace checks passed' in boot_log
            assert b'All IPC checks passed' in boot_log
            if arch == 'amd64':
                # Exercise the real PS/2 IRQ path, independently of COM1 RX.
                with socket.socket(socket.AF_UNIX) as qmp:
                    qmp.connect(str(qmp_path))
                    stream = qmp.makefile('rwb', buffering=0)
                    json.loads(stream.readline())  # greeting

                    def qmp_command(execute, arguments=None):
                        request = {'execute': execute}
                        if arguments is not None:
                            request['arguments'] = arguments
                        stream.write(json.dumps(request).encode() + b'\n')
                        while True:
                            response = json.loads(stream.readline())
                            if 'error' in response:
                                raise AssertionError(response)
                            if 'return' in response:
                                return

                    qmp_command('qmp_capabilities')
                    for key in 'echo keyboard':
                        qcode = 'spc' if key == ' ' else key
                        qmp_command('human-monitor-command', {'command-line': f'sendkey {qcode} 10'})
                        time.sleep(0.04)
                    qmp_command('human-monitor-command', {'command-line': 'sendkey ret 10'})
                    assert b'keyboard' in receive(b'/> ', 5)
                    stream.close()
            command(b'HeLp', b'Commands (case insensitive')
            command(b'ver', b'MMURTL/RS v0.1.0')
            command(b'mem', b'physical memory free')
            command(b'ps', b'cli')
            command(b'md test')
            command(b'path test', prompt=b'/test> ')
            command(b'rd .', b'cannot remove', prompt=b'/test> ')
            command(b'write "a b.txt" "hello shell"', prompt=b'/test> ')
            command(b'type "a b.txt"', b'hello shell', prompt=b'/test> ')
            command(b'copy "a b.txt" copy.txt', prompt=b'/test> ')
            command(b'copy "a b.txt" copy.txt', b'already exists', prompt=b'/test> ')
            command(b'dump copy.txt', b'68 65 6c 6c 6f', prompt=b'/test> ')
            command(b'cd ..')
            command(b'rd test', b'directory not empty')
            command(b'del test', b'use RD')
            command(b'del "test/a b.txt"')
            command(b'del test/copy.txt')
            command(b'rd test')
            command(b'cd /does-not-exist', b'no such directory')
            command(b'type missing', b'file not found')
            command(b'write bad.elf "not an ELF"')
            command(b'run bad.elf', b'Error:')
            command(b'run hello', b'longest Collatz chain')
            command(b'hello', b'longest Collatz chain')
            command(b'run /BIN/HELLO.ELF', b'longest Collatz chain')
            command(b'echo wrong\x7f\x7f\x7f\x7f\x7fright', b'right')
            command(b'type "unfinished', b'unclosed quote')
            command(b'echo ' + b'x' * 256, b'command too long; discarded')
            command(b'echo recovery', b'recovery')
            proc.stdin.write(b'echo cancelled\x03')
            proc.stdin.flush()
            assert b'^C' in receive(b'/> ', 5)
            # CRLF must produce one prompt, without a spurious empty command.
            proc.stdin.write(b'echo crlf\r\n')
            proc.stdin.flush()
            receive(b'/> ', 5)
            command(b'echo after-crlf', b'after-crlf')
            print(f'{arch}, {cpus} CPUs: shell, editing, exFAT, and foreground ELF tests passed', flush=True)
        finally:
            proc.terminate()
            proc.wait(timeout=5)
            log_path = ROOT / 'target' / f'shell-test-{arch}-{cpus}.log'
            log_path.write_bytes(log)
            print(f'Log: {log_path}', flush=True)
        subprocess.run(['fsck.exfat', '-n', str(disk)], check=True, stdout=subprocess.DEVNULL)


if __name__ == '__main__':
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument('--arch', choices=['amd64', 'arm64'], required=True)
    parser.add_argument('--cpus', type=int, default=4, choices=[1, 2, 4, 8])
    options = parser.parse_args()
    smoke(options.arch, options.cpus)
