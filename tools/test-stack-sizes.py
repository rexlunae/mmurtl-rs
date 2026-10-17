#!/usr/bin/env python3
"""Reject oversized optimized Rust stack frames on both kernel targets.

This bounds individual static frames, not cumulative call depth or assembly
entry frames. Early ARM boot has one explicit 8 KiB allowance on its 64 KiB
bootstrap stack; ordinary kernel frames are limited to 4 KiB.
"""
from pathlib import Path
import re
import subprocess
ROOT=Path(__file__).resolve().parent.parent
def main():
    sysroot=subprocess.check_output(['rustc','--print','sysroot'],text=True).strip()
    host=next(x.split(': ',1)[1] for x in subprocess.check_output(['rustc','-vV'],text=True).splitlines() if x.startswith('host:'))
    tool=Path(sysroot)/'lib/rustlib'/host/'bin/llvm-readobj'
    for target in ['x86_64-unknown-none','aarch64-unknown-none-softfloat']:
        output=subprocess.check_output([str(tool),'--stack-sizes','--demangle',str(ROOT/'target'/target/'release/mmurtl-rs')],text=True)
        rows=[(name,int(size,16)) for name,size in re.findall(r'Functions: \[(.*?)\]\s+Size: (0x[0-9a-fA-F]+)',output)]
        assert len(rows)>300,(target,'missing stack-size metadata')
        for name,size in rows:
            limit=8192 if target.startswith('aarch64') and name=='arm64_boot_main' else 4096
            assert size<=limit,(target,name,size,limit)
        print(f'{target}: {len(rows)} emitted stack frames within budgets')
if __name__=='__main__': main()
