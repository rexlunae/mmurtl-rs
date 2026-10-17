#!/usr/bin/env python3
"""Check optimized indirect/return gateways, entry ordering and exceptions.

This is a code-shape audit, not a hardware leakage test.
"""
from pathlib import Path
import re
import subprocess
ROOT=Path(__file__).resolve().parent.parent
def main():
    sysroot=Path(subprocess.check_output(['rustc','--print','sysroot'],text=True).strip())
    host=next(x.split(': ',1)[1] for x in subprocess.check_output(['rustc','-vV'],text=True).splitlines() if x.startswith('host:'))
    binary=sysroot/'lib/rustlib'/host/'bin'
    path=ROOT/'target/x86_64-unknown-none/release/mmurtl-rs'
    symbols={name:int(address,16) for address,_,name in re.findall(r'^([0-9a-f]+) (\w) (.+)$',subprocess.check_output([str(binary/'llvm-nm'),'-n',str(path)],text=True),re.M)}
    dump=subprocess.check_output([str(binary/'llvm-objdump'),'-d','--no-show-raw-insn',str(path)],text=True)
    insns=[(int(address,16),op,args.strip()) for address,op,args in re.findall(r'^\s*([0-9a-f]+):[ \t]+([a-z][a-z0-9.]*)[ \t]*([^\n]*)$',dump,re.M)]
    def between(start,end):return [(a,o,s) for a,o,s in insns if symbols[start]<=a<symbols[end]]
    transfers=0;returns=0
    for i,(address,op,args) in enumerate(insns):
        if op in ['call','callq','jmp','jmpq'] and '*' in args:
            assert symbols['ap_trampoline_start']<=address<symbols['ap_trampoline_end'],(hex(address),'unprotected indirect branch',args)
        if op in ['jmp','jmpq'] and '<__x86_return_thunk>' in args:transfers+=1
        if op not in ['ret','retq']:continue
        returns+=1
        assert address%64>=32,'Intel RET outside upper cacheline half'
        assert (symbols['x86_clear_bhb']<=address<symbols['x86_fill_rsb'] or symbols['__x86_generic_return']<=address<=symbols['__x86_plain_return']),(hex(address),'bare return outside approved reference sequences')
        assert insns[i+1][1]=='int3','return lacks SLS trap'
    assert transfers>500 and returns==4,(transfers,returns)
    registers=['rax','rcx','rdx','rbx','rbp','rsi','rdi',*[f'r{i}' for i in range(8,16)]]
    for register in registers:
        start=symbols['__x86_indirect_thunk_'+register]
        body=[(op,args) for a,op,args in insns if start<=a<start+128]
        assert body[0][0]=='endbr64'
        assert any(op.startswith('cmp') and 'X86_RETPOLINE_LANDING' in args for op,args in body)
        assert any(op=='lfence' for op,_ in body) and any(op=='pause' for op,_ in body)
        assert any(op.startswith('jmp') and '__x86_return_thunk' in args for op,args in body)
    for name,end in [('__x86_return_patch','__x86_untrain_entry'),('__x86_untrain_patch','__x86_generic_return')]:
        assert symbols[name]%8==0
        assert between(name,end)[0][1] in ['jmp','jmpq']
    entry=between('x86_predictor_enter','x86_clear_bhb')
    assert any(op.startswith('call') and '__x86_untrain_entry' in args for _,op,args in entry)
    assert any(op=='wrmsr' for _,op,_ in entry)
    assert any(op=='cpuid' for _,op,_ in entry),'entry decisions lack serializing barrier'
    for start,end in [('syscall_int_entry','syscall_dispatch'),('timer_handler','schedule_and_switch'),('x86_exception_common','x86_exception_dispatch')]:
        body=[args for a,op,args in insns if a>=symbols[start] and op.startswith('call')]
        assert next(i for i,args in enumerate(body) if 'x86_predictor_enter' in args)<next(i for i,args in enumerate(body) if end in args)
    print(f'x86 predictor audit: {transfers} return gateways, 15 landing-checked retpolines, entry untraining, four approved RET sites')
if __name__=='__main__':main()
