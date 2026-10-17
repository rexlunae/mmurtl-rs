#!/usr/bin/env python3
"""Boot the security regressions under selectable CPU/firmware configurations.

Build first with make bios (amd64) or make arm64. No writable disks required.
"""
import argparse
import os
from pathlib import Path
import select
import shutil
import subprocess
import tempfile
import time

ROOT = Path(__file__).resolve().parent.parent


def main():
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument('--arch', choices=['amd64', 'arm64'], required=True)
    parser.add_argument('--cpus', type=int, default=4)
    parser.add_argument('--cpu')
    parser.add_argument('--uefi', action='store_true')
    parser.add_argument('--el2', action='store_true')
    parser.add_argument('--require', action='append', default=[])
    args = parser.parse_args()
    if args.uefi and args.arch != 'amd64': parser.error('--uefi requires amd64')
    if args.el2 and args.arch != 'arm64': parser.error('--el2 requires arm64')
    with tempfile.TemporaryDirectory(prefix='mmurtl-security-') as temporary:
        if args.arch == 'amd64':
            image = ROOT / 'target' / ('mmurtl-rs-uefi.img' if args.uefi else 'mmurtl-rs-bios.img')
            copy = Path(temporary) / 'boot.img'
            shutil.copyfile(image, copy)
            command = ['qemu-system-x86_64', '-drive', f'format=raw,file={copy}',
                       '-serial', 'stdio', '-display', 'none', '-monitor', 'none']
            if args.uefi:
                command += ['-bios', '/usr/share/ovmf/OVMF.fd']
        else:
            command = ['qemu-system-aarch64', '-machine', 'virt,gic-version=3' + (',virtualization=on' if args.el2 else ''),
                       '-cpu', args.cpu or 'cortex-a72', '-nographic', '-monitor', 'none',
                       '-kernel', str(ROOT / 'target/aarch64-unknown-none-softfloat/release/mmurtl-rs')]
        if args.cpu and args.arch == 'amd64': command += ['-cpu', args.cpu]
        command += ['-m', '256M', '-smp', str(args.cpus)]
        proc = subprocess.Popen(command, stdin=subprocess.DEVNULL, stdout=subprocess.PIPE, stderr=subprocess.STDOUT)
        output = bytearray()
        try:
            deadline = time.monotonic() + 60
            while time.monotonic() < deadline:
                if select.select([proc.stdout], [], [], 0.1)[0]:
                    data = os.read(proc.stdout.fileno(), 65536)
                    if not data: break
                    output.extend(data)
                    if b'/> ' in output: break
            text = output.decode(errors='replace')
            required = ['/> ', 'All userspace checks passed', 'All IPC checks passed', *args.require]
            for marker in required:
                assert marker in text, f'missing {marker!r}:\n{text[-5000:]}'
            assert '!!! KERNEL PANIC' not in text and 'checks FAILED' not in text
            print(f'{args.arch}, {args.cpus} CPUs, {args.cpu or "default"}, {"UEFI" if args.uefi else "EL2" if args.el2 else "default boot"}: security checks passed')
        finally:
            proc.terminate(); proc.wait(timeout=5)
            name = f'security-{args.arch}-{args.cpus}-' + ('uefi' if args.uefi else 'el2' if args.el2 else 'boot')
            (ROOT / 'target' / (name + '.log')).write_bytes(output)


if __name__ == '__main__':
    main()
