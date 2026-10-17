#!/usr/bin/env python3
"""Exercise PIE relocation, per-start ASLR, RELRO and user stack canaries.

Build: make bios arm64 user user-arm64 USER_FEATURES=runtime-tests
"""
import argparse
import os
import re
import select
import subprocess
import tempfile
import time
from pathlib import Path
from make_disk import create_image
ROOT=Path(__file__).resolve().parent.parent
def main():
    parser=argparse.ArgumentParser(description=__doc__)
    parser.add_argument('--arch',choices=['amd64','arm64'],required=True)
    args=parser.parse_args(); target='x86_64-unknown-none' if args.arch=='amd64' else 'aarch64-unknown-none-softfloat'
    programs={name.upper()+('.TEST' if name in ['canary','relro'] else '.ELF'):(ROOT/'user/target'/target/'release'/name).read_bytes() for name in ['hello','sieve','canary','relro']}
    with tempfile.TemporaryDirectory(prefix='mmurtl-user-aslr-') as directory:
        disk=Path(directory)/'disk.img';create_image(disk,programs)
        if args.arch=='amd64':
            command=['qemu-system-x86_64','-cpu','max','-drive',f'format=raw,file={ROOT/"target/mmurtl-rs-bios.img"}',
                '-drive',f'format=raw,file={disk},if=virtio','-display','none','-serial','stdio','-monitor','none','-snapshot']
        else:
            command=['qemu-system-aarch64','-machine','virt,gic-version=3','-cpu','cortex-a72','-kernel',str(ROOT/'target'/target/'release/mmurtl-rs'),
                '-drive',f'if=none,format=raw,file={disk},id=hd0','-device','virtio-blk-device,drive=hd0','-nographic','-monitor','none']
        proc=subprocess.Popen(command+['-smp','4','-m','256M'],stdin=subprocess.PIPE,stdout=subprocess.PIPE,stderr=subprocess.STDOUT)
        log=bytearray()
        def receive(needle,timeout=45):
            start=len(log);deadline=time.monotonic()+timeout
            while time.monotonic()<deadline:
                if select.select([proc.stdout],[],[],.1)[0]:
                    chunk=os.read(proc.stdout.fileno(),65536)
                    if not chunk:break
                    log.extend(chunk)
                    if needle in log[start:]:return bytes(log[start:])
            raise AssertionError(f'missing {needle!r}:\n'+log[-4000:].decode(errors='replace'))
        try:
            boot=receive(b'/> ')
            assert b'All userspace checks passed' in boot and b'All IPC checks passed' in boot
            assert b'sieve: 1229 primes below 10000 (data segment says 8)' in boot,boot[-5000:]
            proc.stdin.write(b'run /BIN/CANARY.TEST\r');proc.stdin.flush();canary=receive(b'/> ',20)
            assert b'[USER CANARY] stack corruption detected; exiting' in canary,canary
            assert b'[USER CANARY] ERROR' not in canary and b'"shell-program" exited' in canary and b'"shell-program" killed' not in canary,canary
            proc.stdin.write(b'run /BIN/RELRO.TEST\r');proc.stdin.flush();relro=receive(b'/> ',20)
            assert b'[USER RELRO] ERROR' not in relro,relro
            assert re.search(rb'"shell-program" killed: (?:#PF protection violation|data abort \(permission fault\))',relro),relro
            assert b'longest Collatz chain' in boot
            entries=[]
            for _ in range(10):
                proc.stdin.write(b'run hello\r');proc.stdin.flush();reply=receive(b'/> ',20)
                assert b'longest Collatz chain' in reply,reply
                match=re.search(rb'"shell-program" TID=\d+ entry=(0x[0-9a-f]+)',reply)
                assert match,reply;entries.append(int(match[1],16))
            assert len(set(entries))==len(entries),entries
            print(args.arch+': 10 PIE restarts have distinct entries; relocated HELLO, RELRO fault and compiler canary exit passed')
        finally:
            proc.terminate();proc.wait(timeout=5)
            (ROOT/f'target/user-aslr-{args.arch}.log').write_bytes(log)
if __name__=='__main__':main()
