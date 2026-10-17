#!/usr/bin/env python3
"""Verify a test-cfi image traps a compiler-checked wrong-type call.

Build: make bios arm64 KERNEL_FEATURES=test-cfi
"""
import argparse
import os
import select
import subprocess
import time
from pathlib import Path
ROOT=Path(__file__).resolve().parent.parent
def main():
    parser=argparse.ArgumentParser(description=__doc__)
    parser.add_argument('--arch',choices=['amd64','arm64'],required=True)
    args=parser.parse_args()
    if args.arch=='amd64':
        command=['qemu-system-x86_64','-cpu','max','-drive',f'format=raw,file={ROOT/"target/mmurtl-rs-bios.img"}','-snapshot']
    else:
        command=['qemu-system-aarch64','-machine','virt,gic-version=3','-cpu','cortex-a72','-kernel',str(ROOT/'target/aarch64-unknown-none-softfloat/release/mmurtl-rs')]
    proc=subprocess.Popen(command+['-smp','4','-m','256M','-display','none','-serial','stdio','-monitor','none','-no-reboot'],stdout=subprocess.PIPE,stderr=subprocess.STDOUT)
    output=bytearray()
    try:
        deadline=time.monotonic()+45
        while time.monotonic()<deadline and proc.poll() is None:
            if select.select([proc.stdout],[],[],.1)[0]:
                output.extend(os.read(proc.stdout.fileno(),65536))
                if b'[CFI] fatal KCFI' in output and b'; CPU halted' in output:break
        text=output.decode(errors='replace')
        assert '[CFI] fatal KCFI at (ptr) target (ptr); CPU halted' in text,text[-4000:]
        assert '[CFI TEST] ERROR' not in text,text[-4000:]
        print(args.arch+': wrong-type call trapped before target execution, PC/target diagnostics restricted')
    finally:
        proc.terminate();proc.wait(timeout=5)
        (ROOT/f'target/cfi-test-{args.arch}.log').write_bytes(output)
if __name__=='__main__':main()
