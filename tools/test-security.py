#!/usr/bin/env python3
"""Boot the security regressions under selectable CPU/firmware configurations.

Build first with make bios (amd64) or make arm64. No writable disks required.
"""
import argparse
import os
import json
import socket
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
    parser.add_argument('--bootargs', help='mitigation policy / kernel boot arguments')
    parser.add_argument('--uefi', action='store_true')
    parser.add_argument('--el2', action='store_true')
    parser.add_argument('--nmi', action='store_true', help='inject an amd64 NMI and verify the shell remains responsive')
    parser.add_argument('--no-firmware-seed', action='store_true', help='disable QEMU arm64 DT seed properties')
    parser.add_argument('--rng', action='store_true', help='attach a host-backed virtio entropy device')
    parser.add_argument('--require', action='append', default=[])
    args = parser.parse_args()
    if args.uefi and args.arch != 'amd64': parser.error('--uefi requires amd64')
    if args.no_firmware_seed and args.arch != 'arm64': parser.error('--no-firmware-seed requires arm64')
    if args.el2 and args.arch != 'arm64': parser.error('--el2 requires arm64')
    if args.nmi and args.arch != 'amd64': parser.error('--nmi requires amd64')
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
            command = ['qemu-system-aarch64', '-machine', 'virt,gic-version=3' + (',virtualization=on' if args.el2 else '') + (',dtb-randomness=off' if args.no_firmware_seed else ''),
                       '-cpu', args.cpu or 'cortex-a72', '-nographic', '-monitor', 'none',
                       '-kernel', str(ROOT / 'target/aarch64-unknown-none-softfloat/release/mmurtl-rs')]
        if args.cpu and args.arch == 'amd64': command += ['-cpu', args.cpu]
        qmp_path = str(Path(temporary) / 'qmp.sock')
        if args.nmi: command += ['-qmp', f'unix:{qmp_path},server=on,wait=off']
        if args.bootargs is not None:
            if args.arch == 'amd64':
                bootargs_path = Path(temporary) / 'bootargs'
                bootargs_path.write_text(args.bootargs)
                command += ['-fw_cfg', f'name=opt/mmurtl/bootargs,file={bootargs_path}']
            else:
                command += ['-append', args.bootargs]
        if args.rng: command += ['-device', 'virtio-rng-pci' if args.arch == 'amd64' else 'virtio-rng-device']
        command += ['-m', '256M', '-smp', str(args.cpus)]
        proc = subprocess.Popen(command, stdin=subprocess.PIPE, stdout=subprocess.PIPE, stderr=subprocess.STDOUT)
        output = bytearray()
        try:
            deadline = time.monotonic() + 60
            while time.monotonic() < deadline:
                if select.select([proc.stdout], [], [], 0.1)[0]:
                    data = os.read(proc.stdout.fileno(), 65536)
                    if not data: break
                    output.extend(data)
                    if b'/> ' in output: break
            if args.nmi and b'/> ' in output:
                with socket.socket(socket.AF_UNIX) as qmp:
                    qmp.connect(qmp_path)
                    stream = qmp.makefile('rwb', buffering=0)
                    json.loads(stream.readline())
                    for execute in ['qmp_capabilities', 'inject-nmi']:
                        stream.write((json.dumps({'execute': execute}) + '\n').encode())
                        while True:
                            response = json.loads(stream.readline())
                            assert 'error' not in response, response
                            if 'return' in response: break
                proc.stdin.write(b'ECHO NMI-SURVIVED\n'); proc.stdin.flush()
                deadline = time.monotonic() + 5
                while time.monotonic() < deadline:
                    if select.select([proc.stdout], [], [], 0.1)[0]:
                        output.extend(os.read(proc.stdout.fileno(), 65536))
                        if b'NMI-SURVIVED\r\n/>' in output or b'NMI-SURVIVED\n/>' in output: break
                assert output.count(b'NMI-SURVIVED') >= 2, 'shell did not respond after NMI'
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
