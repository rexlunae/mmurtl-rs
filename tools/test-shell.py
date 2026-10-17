#!/usr/bin/env python3
"""Boot-test the CLI with a disposable exFAT disk; never touches user disks.

Build first: make bios user (amd64), or make arm64 user-arm64 (arm64).
Requires QEMU and exfatprogs, or dosfstools + rustc for FAT images.
Uses software emulation.
"""
import argparse
import json
import os
from pathlib import Path
import select
import shutil
import socket
import subprocess
import tempfile
import time

from make_disk import create_image

ROOT = Path(__file__).resolve().parent.parent



def smoke(arch, cpus, filesystem):
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
    dependencies = ['mkfs.exfat', 'fsck.exfat'] if filesystem == 'exfat' else ['mkfs.fat', 'fsck.fat', 'rustc']
    for tool in [args[0], *dependencies]:
        assert shutil.which(tool), f'{tool} is required'
    with tempfile.TemporaryDirectory(prefix='mmurtl-shell-') as directory:
        disk = Path(directory) / 'disk.img'
        if filesystem == 'exfat':
            create_image(disk, {'HELLO.ELF': program.read_bytes()})
        else:
            bits = filesystem[3:]
            with disk.open('wb') as stream:
                stream.truncate({'12': 1440 * 1024, '16': 16 * 1024 * 1024, '32': 64 * 1024 * 1024}[bits])
            subprocess.run(['mkfs.fat', '-F', bits, str(disk)], check=True, stdout=subprocess.DEVNULL)
            seed = Path(directory) / 'seed-fat'
            subprocess.run(['rustc', '--edition=2021', str(ROOT / 'tools/tests/fat_driver.rs'), '-o', str(seed)], check=True)
            subprocess.run([str(seed), str(disk), '--seed', str(program)], check=True)
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
            command(b'rd /TEST', b'cannot remove', prompt=b'/test> ')
            command(b'write "a b.txt" "hello shell"', prompt=b'/test> ')
            command(b'type "a b.txt"', b'hello shell', prompt=b'/test> ')
            command(b'copy "a b.txt" copy.txt', prompt=b'/test> ')
            command(b'copy "a b.txt" copy.txt', b'already exists', prompt=b'/test> ')
            command(b'dump copy.txt', b'68 65 6c 6c 6f', prompt=b'/test> ')
            if filesystem != 'exfat':
                command('write "café 😀.txt" "unicode data"'.encode(), prompt=b'/test> ')
                command('type "CAFÉ 😀.TXT"'.encode(), b'unicode data', prompt=b'/test> ')
                command('del "café 😀.txt"'.encode(), prompt=b'/test> ')
            command(b'cd ..')
            command(b'rd test', b'directory not empty')
            command(b'del test', b'use RD')
            command(b'del "test/a b.txt"')
            command(b'del test/copy.txt')
            command(b'rd test')
            if filesystem != 'exfat':
                command(b'md "Long current directory"')
                command(b'cd "Long current directory"', prompt=b'/Long current directory> ')
                command(b'rd /LONGCU~1', b'cannot remove', prompt=b'/Long current directory> ')
                command(b'cd /')
                command(b'rd "Long current directory"')
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
            print(f'{arch}, {cpus} CPUs, {filesystem}: shell, editing, filesystem, and foreground ELF tests passed', flush=True)
        finally:
            proc.terminate()
            proc.wait(timeout=5)
            log_path = ROOT / 'target' / f'shell-test-{arch}-{cpus}-{filesystem}.log'
            log_path.write_bytes(log)
            print(f'Log: {log_path}', flush=True)
        subprocess.run(['fsck.exfat' if filesystem == 'exfat' else 'fsck.fat', '-n', str(disk)], check=True, stdout=subprocess.DEVNULL)


if __name__ == '__main__':
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument('--arch', choices=['amd64', 'arm64'], required=True)
    parser.add_argument('--cpus', type=int, default=4, choices=[1, 2, 4, 8])
    parser.add_argument('--filesystem', choices=['exfat', 'fat12', 'fat16', 'fat32'], default='exfat')
    options = parser.parse_args()
    smoke(options.arch, options.cpus, options.filesystem)
