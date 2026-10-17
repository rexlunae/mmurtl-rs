#!/usr/bin/env python3
"""Verify a deliberate kernel CFI violation traps before target execution.

Build: make bios arm64 KERNEL_FEATURES=test-cfi
For landing-pad tests, build with test-bti (ARM) or test-ibt (x86), then
select --kind bti/ibt. Unsupported hardware requires --allow-unavailable
and is reported as a skip, never as positive enforcement evidence.
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
    parser.add_argument('--kind',choices=['kcfi','bti','ibt','canary','pac'],default='kcfi')
    parser.add_argument('--allow-unavailable',action='store_true')
    parser.add_argument('--cpu')
    args=parser.parse_args()
    if args.kind=='bti' and args.arch!='arm64' or args.kind=='ibt' and args.arch!='amd64':parser.error('landing test kind does not match architecture')
    if args.arch=='amd64':
        command=['qemu-system-x86_64','-cpu',args.cpu or 'max','-drive',f'format=raw,file={ROOT/"target/mmurtl-rs-bios.img"}','-snapshot']
    else:
        command=['qemu-system-aarch64','-machine','virt,gic-version=3','-cpu',args.cpu or ('max' if args.kind in ['bti','pac'] else 'cortex-a72'),'-kernel',str(ROOT/'target/aarch64-unknown-none-softfloat/release/mmurtl-rs')]
    proc=subprocess.Popen(command+['-smp','4','-m','256M','-display','none','-serial','stdio','-monitor','none','-no-reboot'],stdout=subprocess.PIPE,stderr=subprocess.STDOUT)
    output=bytearray()
    try:
        deadline=time.monotonic()+45
        while time.monotonic()<deadline and proc.poll() is None:
            if select.select([proc.stdout],[],[],.1)[0]:
                output.extend(os.read(proc.stdout.fileno(),65536))
                if b'[CFI] fatal ' in output and b'; CPU halted' in output or b'positive test skipped' in output:break
        text=output.decode(errors='replace')
        if 'positive test skipped' in text:
            assert args.kind!='kcfi' and args.allow_unavailable,text[-4000:]
            print(args.arch+': '+args.kind+' unavailable; positive enforcement test explicitly skipped');return
        kind={'kcfi':'KCFI','bti':'BTI','ibt':'CET-IBT/#CP','canary':'stack canary','pac':'PAC authentication'}[args.kind]
        target='(unknown)' if args.kind in ['canary','pac'] else '(ptr)'
        assert f'[CFI] fatal {kind} at (ptr) target {target}; CPU halted' in text,text[-4000:]
        assert '[CFI TEST] ERROR' not in text,text[-4000:]
        print(args.arch+': '+args.kind+' violation trapped before target execution, PC/target diagnostics restricted')
    finally:
        proc.terminate();proc.wait(timeout=5)
        (ROOT/f'target/cfi-test-{args.arch}.log').write_bytes(output)
if __name__=='__main__':main()
