#!/usr/bin/env python3
"""Audit per-CPU BHB tables: slot bounds, loop counts, firmware and register saves."""
from pathlib import Path
import re
import subprocess
ROOT=Path(__file__).resolve().parent.parent

def main():
    sysroot=subprocess.check_output(['rustc','--print','sysroot'],text=True).strip()
    host=next(x.split(': ',1)[1] for x in subprocess.check_output(['rustc','-vV'],text=True).splitlines() if x.startswith('host:'))
    bindir=Path(sysroot)/'lib/rustlib'/host/'bin'
    path=ROOT/'target/aarch64-unknown-none-softfloat/release/mmurtl-rs'
    symbols={name:int(addr,16) for addr,_,name in re.findall(r'^([0-9a-f]+) (\w) (.+)$',subprocess.check_output([str(bindir/'llvm-nm'),'-n',str(path)],text=True),re.M)}
    dump=subprocess.check_output([str(bindir/'llvm-objdump'),'-d','--no-show-raw-insn',str(path)],text=True)
    insns=[(int(a,16),op,arg.split('//',1)[0].strip()) for a,op,arg in re.findall(r'^[ \t]*([0-9a-f]+):[ \t]+([a-z][a-z0-9.]*)[ \t]*([^\n]*)$',dump,re.M)]
    tables=[('predictor_loop'+str(k),'loop',k,0,None) for k in [8,24,32,38,132]]+[
        ('predictor_clrbhb','clear',0,0,None),('predictor_wa3_hvc','plain',0,3,'hvc'),('predictor_wa3_smc','plain',0,3,'smc')]
    for base,mode,k in [('plain','plain',0),('clrbhb','clear',0)]+[(f'loop{k}','loop',k) for k in [8,24,32,38,132]]:
        for conduit in ['hvc','smc']: tables.append((f'predictor_{base}_wa1_{conduit}',mode,k,1,conduit))
    for name,mode,k,fw,conduit in tables:
        start=symbols[name]
        assert start%2048==0,name+' alignment'
        for slot in range(16):
            address=start+slot*128
            code=[(op,arg) for a,op,arg in insns if address<=a<address+128]
            assert code[0][0]=='sub',(name,slot,'missing saved frame')
            save=next(i for i,(op,arg) in enumerate(code) if op=='stp' and arg=='x0, x1, [sp]')
            assert 'sp, sp, #0x110' in code[0][1],(name,slot)
            if slot==4:
                assert any('arm64_stack_overflow' in arg for op,arg in code[1:save]),(name,slot,'missing stackless overflow gate')
                assert not any(op in ['str','stp','ldr','ldp','bl'] for op,_ in code[1:save]),(name,slot,'stack access before guard gate')
            else: assert save==1,(name,slot)
            end=next((i for i,(op,arg) in enumerate(code) if op=='b' and 'exception_common' in arg),None)
            assert end is not None,(name,slot,'slot exceeded/invalid common handoff')
            assert all(op=='udf' for op,_ in code[end+1:]),(name,slot,'nonzero slot padding')
            code=code[:end+1]
            ops=[op for op,_ in code]
            if slot<8:
                assert len(code[save:])==3,(name,slot,'kernel vector must remain plain after guard check')
            elif mode=='loop':
                assert any(op=='mov' and re.fullmatch(r'x0, #0x'+f'{k:x}',arg) for op,arg in code),(name,slot,'wrong loop count')
                assert 'subs' in ops and 'b.ne' in ops and 'dsb' in ops and 'isb' in ops,(name,slot)
            elif mode=='clear':
                assert any(op=='clrbhb' or op=='hint' and '#0x16' in arg for op,arg in code),(name,slot)
                assert 'isb' in ops,(name,slot)
            if slot>=8 and fw:
                call=ops.index(conduit)
                assert any(op=='stp' and arg=='x2, x3, [sp, #0x10]' for op,arg in code[:call]),(name,slot,'unsaved volatile registers')
                assert any(op=='ldp' and arg=='x2, x3, [sp, #0x10]' for op,arg in code[call+1:]),(name,slot,'unrestored volatile registers')
                for reg in [1,2,3]: assert ('mov',f'x{reg}, xzr') in code[:call],(name,slot,'unsanitized firmware arguments')
                assert any(op=='mov' and arg==('w0, #0x8000' if fw==1 else 'w0, #0x3fff') for op,arg in code[:call]),(name,slot,'wrong workaround')
                assert any(op=='movk' and arg=='w0, #0x8000, lsl #16' for op,arg in code[:call]),(name,slot,'wrong service class')
        assert symbols['predictor_vectors_end']>=start+2048
    sized=subprocess.check_output([str(bindir/'llvm-nm'),'-S',str(path)],text=True)
    size=int(next(x.split()[1] for x in sized.splitlines() if x.endswith(' arm64_user_return_policy')),16)
    start=symbols['arm64_user_return_policy']
    assert any(op=='bl' and 'arm64_predictor_user_return' in arg for address,op,arg in insns if start<=address<start+size), 'missing every-return firmware hook'
    print(f'ARM predictor vectors: {len(tables)} tables / {len(tables)*16} bounded slots, loop lengths, firmware registers/arguments and every-return hook verified')

if __name__=='__main__':main()
