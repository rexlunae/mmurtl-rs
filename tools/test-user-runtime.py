#!/usr/bin/env python3
"""Audit compiler-emitted canary checks and pre-Rust guard initialization.

Build: make user user-arm64 USER_FEATURES=runtime-tests
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
        path=ROOT/'user/target'/target/'release/canary';data=path.read_bytes()
        assert struct.unpack_from('<H',data,16)[0]==3,'runtime is not PIE'
        table=subprocess.check_output([str(bindir/'llvm-nm'),'-S','--demangle',str(path)],text=True)
        symbols={name:(int(addr,16),int(size,16)) for addr,size,name in re.findall(r'^([0-9a-f]+)\s+([0-9a-f]+)\s+\w\s+(.+)$',table,re.M)}
        dump=subprocess.check_output([str(bindir/'llvm-objdump'),'-d','--no-show-raw-insn','--demangle',str(path)],text=True)
        instructions=[(int(addr,16),op,args) for addr,op,args in re.findall(r'^\s*([0-9a-f]+):[ \t]+([a-z][a-z0-9.]*)[ \t]*([^\n]*)$',dump,re.M)]
        def body(name):
            start,size=symbols[name];assert size>0,(target,name,'missing symbol size')
            return [(op,args) for addr,op,args in instructions if start<=addr<start+size]
        guard=symbols['__stack_chk_guard'][0];failure=symbols['__stack_chk_fail'][0]
        protected=body('canary::protected_frame');entry=body('_start')
        assert any(op.startswith('cmp') for op,_ in protected),(target,'missing canary comparison')
        assert any(op in ['jne','b.ne'] for op,_ in protected),(target,'missing failure branch')
        if target.startswith('x86'):
            # x86 PIE calls/loads globals through relocated GOT entries.
            headers=[struct.unpack_from('<IIQQQQQQ',data,struct.unpack_from('<Q',data,32)[0]+i*struct.unpack_from('<H',data,54)[0]) for i in range(struct.unpack_from('<H',data,56)[0])]
            def file_offset(va):
                for kind,_,offset,start,_,size,_,_ in headers:
                    if kind==1 and start<=va<start+size:return offset+va-start
                raise AssertionError(('unbacked address',hex(va)))
            dynamic=next(h for h in headers if h[0]==2)
            tags=dict(struct.unpack_from('<QQ',data,at) for at in range(dynamic[2],dynamic[2]+dynamic[5],16))
            rela=file_offset(tags[7]);slots={}
            for at in range(rela,rela+tags[8],24):
                destination,info,addend=struct.unpack_from('<QQQ',data,at)
                assert info==8;slots.setdefault(addend,[]).append(destination)
            assert any(op.startswith('call') and any(f'# 0x{slot:x}' in args for slot in slots[failure]) for op,args in protected),(target,'missing compiler failure call')
            assert any(any(f'# 0x{slot:x}' in args for slot in slots[guard]) for _,args in protected),(target,'missing guard load')
            first_call=next(i for i,(op,_) in enumerate(entry) if op.startswith('call'))
            assert any('__stack_chk_guard' in args and op.startswith('mov') for op,args in entry[:first_call]),'guard initialized after protected Rust entry'
            assert not any('%fs:' in args for _,args in protected),'unexpected unconfigured user TLS guard'
        else:
            assert any(op=='bl' and '__stack_chk_fail' in args for op,args in protected),(target,'missing compiler failure call')
            assert any('__stack_chk_guard' in args for _,args in protected),(target,'missing guard load')
            first_call=next(i for i,(op,_) in enumerate(entry) if op=='bl')
            assert any(op=='str' for op,_ in entry[:first_call]),'guard initialized after protected Rust entry'
        print(target+': PIE compiler canary check and pre-Rust guard initialization verified')
if __name__=='__main__':main()
