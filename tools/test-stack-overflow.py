#!/usr/bin/env python3
"""Boot test-stack-overflow images and verify fatal handling on emergency stacks.

Build: make bios arm64 KERNEL_FEATURES=test-stack-overflow
"""
import argparse
import os
import re
import select
import subprocess
import time
from pathlib import Path
ROOT = Path(__file__).resolve().parent.parent
def main():
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument('--arch', choices=['amd64','arm64'], required=True)
    args = parser.parse_args()
    common = ['-smp','4','-m','256M','-display','none','-monitor','none','-serial','stdio','-no-reboot']
    if args.arch == 'amd64':
        command = ['qemu-system-x86_64','-cpu','max','-snapshot','-drive',f'format=raw,file={ROOT / "target/mmurtl-rs-bios.img"}']
    else:
        command = ['qemu-system-aarch64','-machine','virt,gic-version=3','-cpu','cortex-a72','-kernel',str(ROOT/'target/aarch64-unknown-none-softfloat/release/mmurtl-rs')]
    output = bytearray()
    proc = subprocess.Popen(command+common,stdout=subprocess.PIPE,stderr=subprocess.STDOUT)
    try:
        deadline=time.monotonic()+45
        while time.monotonic()<deadline and proc.poll() is None:
            if select.select([proc.stdout],[],[],.1)[0]:
                output.extend(os.read(proc.stdout.fileno(),65536))
                if b'emergency stack active; CPU halted' in output: break
        text=output.decode(errors='replace')
        assert 'Created task "stack_overflow"' in text,text[-3000:]
        assert re.search(r'\[STACK\] DOUBLE FAULT / kernel stack guard hit on CPU\d+ task T[1-9]\d*; emergency stack active; CPU halted',text),text[-3000:]
        assert not re.search(r'0xffff[0-9a-f]{12}',text), 'raw kernel address in default diagnostics'
        print(args.arch+': deliberate guard overflow handled on emergency stack')
    finally:
        proc.terminate(); proc.wait(timeout=5)
        (ROOT/f'target/stack-overflow-{args.arch}.log').write_bytes(output)
if __name__=='__main__': main()
