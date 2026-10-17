#!/usr/bin/env python3
"""Audit strong kernel canaries, ARM PAC and compiler shadow-stack traffic.

Build normal images first: make build arm64.
"""
from pathlib import Path
import re
import struct
import subprocess
ROOT=Path(__file__).resolve().parent.parent
def main():
    sysroot=subprocess.check_output(['rustc','--print','sysroot'],text=True).strip()
    host=next(x.split(': ',1)[1] for x in subprocess.check_output(['rustc','-vV'],text=True).splitlines() if x.startswith('host:'))
    bindir=Path(sysroot)/'lib/rustlib'/host/'bin'
    for target in ['x86_64-unknown-none','aarch64-unknown-none-softfloat']:
        path=ROOT/'target'/target/'release/mmurtl-rs';data=path.read_bytes()
        symbols={name:int(addr,16) for addr,kind,name in re.findall(r'^([0-9a-f]+) (\w) (.+)$',subprocess.check_output([str(bindir/'llvm-nm'),'-n',str(path)],text=True),re.M)}
        guard=symbols['__stack_chk_guard'];failure=symbols['__stack_chk_fail']
        dump=subprocess.check_output([str(bindir/'llvm-objdump'),'-d','--demangle','--no-show-raw-insn',str(path)],text=True)
        insns=[(int(addr,16),op,args.strip()) for addr,op,args in re.findall(r'^\s*([0-9a-f]+):[ \t]+([a-z][a-z0-9.]*)[ \t]*([^\n]*)$',dump,re.M)]
        if target.startswith('x86'):
            # PIE compiler guard/failure references use relocated GOT slots.
            offset=struct.unpack_from('<Q',data,40)[0];size,count=struct.unpack_from('<HH',data,58)
            sections=[struct.unpack_from('<IIQQQQIIQQ',data,offset+i*size) for i in range(count)]
            slots={guard:set(),failure:set()}
            for section in sections:
                if section[1]!=4:continue
                for at in range(section[4],section[4]+section[5],24):
                    destination,info,addend=struct.unpack_from('<QQq',data,at)
                    if addend in slots:slots[addend].add(destination)
            refs=lambda address:[(op,args) for _,op,args in insns if any(re.search(r'# 0x'+f'{slot:x}'+r'\b',args) for slot in slots[address])]
            assert len(refs(guard))>100,'missing global compiler canary loads'
            failure_calls=sum(op.startswith('call') for op,_ in refs(failure))
            for i,(_,op,args) in enumerate(insns):
                if not op.startswith('mov') or not any(re.search(r'# 0x'+f'{slot:x}'+r'\b',args) for slot in slots[failure]):continue
                register=re.search(r',\s*%(r\w+)\s*#',args)
                if register and any(nextop.startswith('call') and '<__x86_indirect_thunk_'+register[1]+'>' in operands for _,nextop,operands in insns[i+1:i+4]):failure_calls+=1
            assert failure_calls>100,'missing direct or retpoline compiler failure calls'
            assert '%fs:' not in dump,'unexpected uninitialized TLS guard'
        else:
            assert sum('__stack_chk_guard' in args for _,_,args in insns)>100
            assert sum(op=='bl' and '__stack_chk_fail' in args for _,op,args in insns)>100
            assert sum(op=='paciasp' for _,op,_ in insns)>300,'missing compiler PAC signing'
            assert sum(op=='autiasp' for _,op,_ in insns)>300,'missing compiler PAC authentication'
            pushes=sum(op=='str' and re.fullmatch(r'x30, \[x18\], #(?:0x8|8)',args) is not None for _,op,args in insns)
            pops=sum(op=='ldr' and re.fullmatch(r'x30, \[x18, #(?:-0x8|-8)\]!',args) is not None for _,op,args in insns)
            assert pushes>200 and pops>200,('missing compiler shadow stack',pushes,pops)
            assert not any(op=='mov' and args=='x18, xzr' for _,op,args in insns),'reserved x18 cleared'
            for name in ['arm64_enable_boot_pac','arm64_restore_kernel_pac','arm64_clear_user_pac','arm64_secondary_pac_entry']:
                assert next(op for addr,op,_ in insns if addr==symbols[name])=='bti',(name,'missing assembly landing pad')
        print(target+': compiler global canaries and return-protection sequences verified')
if __name__=='__main__':main()
