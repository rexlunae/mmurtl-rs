#!/usr/bin/env python3
"""Audit optimized kernel register-clearing entries and SLS transfer barriers.

This checks emitted code, not hardware leakage. Build both kernels first.
The mixed-mode x86 AP trampoline is excluded from the whole-text decode;
its 64-bit indirect handoff is checked separately.
"""
from pathlib import Path
import re
import subprocess
ROOT = Path(__file__).resolve().parent.parent

def main():
    sysroot = subprocess.check_output(['rustc','--print','sysroot'],text=True).strip()
    host = next(x.split(': ',1)[1] for x in subprocess.check_output(['rustc','-vV'],text=True).splitlines() if x.startswith('host:'))
    binary_dir = Path(sysroot)/'lib/rustlib'/host/'bin'
    for target in ['x86_64-unknown-none','aarch64-unknown-none-softfloat']:
        path = ROOT/'target'/target/'release/mmurtl-rs'
        dump = subprocess.check_output([str(binary_dir/'llvm-objdump'),'-d','--no-show-raw-insn',str(path)],text=True)
        symbols = {name:int(addr,16) for addr,kind,name in re.findall(r'^([0-9a-f]+) (\w) (.+)$',subprocess.check_output([str(binary_dir/'llvm-nm'),'-n',str(path)],text=True),re.M)}
        insns = [(int(addr,16),op,args.strip()) for addr,op,args in re.findall(r'^[ \t]*([0-9a-f]+):[ \t]+([a-z][a-z0-9.]*)[ \t]*([^\n]*)$',dump,re.M)]
        def before_call(name, callee):
            start=symbols[name]
            body=[]
            for address,op,args in insns:
                if address < start: continue
                if op in ['call','callq','bl'] and callee in args: return body
                body.append((address,op,args))
            raise AssertionError('missing entry call: '+name+' -> '+callee)
        checked=0
        if target.startswith('x86'):
            all_regs={'eax','ecx','edx','ebx','ebp','esi','edi',*(f'r{i}d' for i in range(8,16))}
            for name,callee,args in [('syscall_int_entry','syscall_dispatch',{'edi'}),('timer_handler','schedule_and_switch',{'edi'}),('yield_handler','yield_and_switch',{'edi'}),('x86_exception_common','x86_exception_dispatch',{'edi','esi','edx'})]:
                body=before_call(name,callee)
                clears={m[1] for _,op,operand in body if op in ['xor','xorl'] and (m:=re.fullmatch(r'%(\w+),\s*%\1',operand))}
                assert all_regs-args <= clears, (name,'uncleared registers',all_regs-args-clears)
                assert any(op=='cld' for _,op,_ in body),name+' missing CLD'
                first=next(i for i,(_,op,_) in enumerate(body) if op in ['xor','xorl'])
                assert sum(op in ['push','pushq'] for _,op,_ in body[:first])==15,name+' incomplete saved GPR frame'
            lo,hi=symbols['ap_trampoline_start'],symbols['ap_trampoline_end']
            for i,(address,op,args) in enumerate(insns):
                transfer=op in ['ret','retq','iret','iretq','lret','lretq'] or op in ['jmp','jmpq'] and '*' in args
                if not transfer: continue
                if lo<=address<hi:
                    if op in ['jmp','jmpq'] and '*%rax' in args:
                        assert insns[i+1][1]=='int3','AP handoff lacks SLS barrier'
                        checked+=1
                    continue
                assert insns[i+1][1]=='int3',(hex(address),op,args,'missing INT3')
                checked+=1
        else:
            body=before_call('exception_common','arm64_exception')
            clears={int(m[1]) for _,op,args in body if op=='mov' and (m:=re.fullmatch(r'x(\d+),\s*xzr',args))}
            assert set(range(2,31))<=clears,('ARM uncleared registers',set(range(2,31))-clears)
            for i,(address,op,args) in enumerate(insns):
                if op not in ['ret','eret','br','blr']: continue
                after=insns[i+1:i+3]
                assert after and (after[0][1]=='sb' or len(after)==2 and after[0][1]=='dsb' and after[1][1]=='isb'),(hex(address),op,args,'missing SB or DSB/ISB')
                checked+=1
        assert checked>100,'unexpectedly small transfer coverage'
        print(f'{target}: register-clearing entries and {checked} SLS transfer barriers verified')

if __name__=='__main__': main()
