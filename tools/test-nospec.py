#!/usr/bin/env python3
"""Check host bounds and selected optimized kernel speculation sequences.

Build first with make build arm64. This is a regression guard for known
sites, not a whole-kernel gadget scanner or a hardware side-channel proof.
"""
from pathlib import Path
import re
import subprocess
import tempfile

ROOT = Path(__file__).resolve().parent.parent

def main():
    with tempfile.TemporaryDirectory(prefix='mmurtl-nospec-') as temporary:
        binary = str(Path(temporary) / 'bounds')
        subprocess.run(['rustc', '--edition=2021', str(ROOT / 'tools/tests/nospec.rs'), '-o', binary], check=True)
        subprocess.run([binary], check=True)
    sysroot = subprocess.check_output(['rustc', '--print', 'sysroot'], text=True).strip()
    host = next(line.split(': ', 1)[1] for line in subprocess.check_output(['rustc', '-vV'], text=True).splitlines() if line.startswith('host:'))
    objdump = Path(sysroot) / 'lib/rustlib' / host / 'bin/llvm-objdump'
    for target in ['x86_64-unknown-none', 'aarch64-unknown-none-softfloat']:
        disassembly = subprocess.check_output([str(objdump), '-d', '--demangle', '--no-show-raw-insn', str(ROOT / 'target' / target / 'release/mmurtl-rs')], text=True)
        functions = {}
        for block in disassembly.split('\n\n'):
            header = re.match(r'[0-9a-f]+ <([^>]+)>:', block)
            if header: functions[header[1]] = block
        names = ['syscall::dispatch', 'memory::user::copy_from_user', 'memory::user::copy_to_user']
        for name in names:
            body = functions['mmurtl_rs::' + name]
            if name == 'syscall::dispatch':
                if target.startswith('x86'):
                    # CET builds disable jump tables. Require the mask and
                    # serializing barrier before the comparison tree's first
                    # branch after masking (the early range reject is safe).
                    mask = re.search(r'\bsbbq?\b', body)
                    assert mask, f'{target}: missing dispatch mask'
                    branch = re.search(r'\bj[a-z]+\s', body[mask.end():])
                    assert branch, f'{target}: missing dispatch comparison tree'
                    body = body[:mask.end() + branch.start()]
                else:
                    branch = re.search(r'\bbr\s+x', body)
                    assert branch, f'{target}: expected dispatch indirect branch not found; update audit for new codegen'
                    body = body[:branch.start()]
            masks = len(re.findall(r'\bsbbq?\b' if target.startswith('x86') else r'\bcsetm\b', body))
            assert masks >= (1 if name == 'syscall::dispatch' else 2), f'{target} {name}: missing bounds masks'
            if target.startswith('aarch64'):
                assert len(re.findall(r'\bcsdb\b', body)) >= masks, f'{name}: missing CSDB'
            if name == 'syscall::dispatch':
                assert ('cpuid' in body if target.startswith('x86') else 'dsb\tsy' in body and 'isb' in body), f'{name}: missing dispatch speculation barrier'
        print(f'{target}: dispatch and user-copy masks/barriers verified')

if __name__ == '__main__':
    main()
