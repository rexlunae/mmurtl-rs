#!/usr/bin/env python3
"""Audit emitted KCFI traps/type prefixes and kernel branch landing pads."""
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
        dump=subprocess.check_output([str(bindir/'llvm-objdump'),'-d','--no-show-raw-insn',str(path)],text=True)
        instructions={int(addr,16):(op,args.strip()) for addr,op,args in re.findall(r'^\s*([0-9a-f]+):[ \t]+([.a-z][a-z0-9.]*)[ \t]*([^\n]*)$',dump,re.M)}
        for address,value in re.findall(r'^\s*([0-9a-f]+):[^\n]*\t\.word\t([^\n]*)$',dump,re.M):
            instructions[int(address,16)]=('.word',value.strip())
        headers=[struct.unpack_from('<IIQQQQQQ',data,struct.unpack_from('<Q',data,32)[0]+i*struct.unpack_from('<H',data,54)[0]) for i in range(struct.unpack_from('<H',data,56)[0])]
        def raw(va,size):
            for kind,_,offset,start,_,filesz,_,_ in headers:
                if kind==1 and start<=va and va+size<=start+filesz:return data[offset+va-start:offset+va-start+size]
            raise AssertionError(('unbacked kernel address',hex(va)))
        section_offset=struct.unpack_from('<Q',data,40)[0]
        section_size,section_count=struct.unpack_from('<HH',data,58)
        sections=[struct.unpack_from('<IIQQQQIIQQ',data,section_offset+i*section_size) for i in range(section_count)]
        relocation_targets={struct.unpack_from('<QQq',data,at)[2] for section in sections if section[1]==4 for at in range(section[4],section[4]+section[5],24)}
        if target.startswith('x86'):
            prefixes=[name for name in symbols if name.startswith('__cfi_')]
            assert len(prefixes)>300,'missing compiler type prefixes'
            for prefix in prefixes:
                name=prefix[len('__cfi_'):];assert name in symbols,(prefix,'missing function')
                assert 5<=symbols[name]-symbols[prefix]<=32,(name,'unexpected type-prefix extent')
                assert raw(symbols[name]-5,1)==b'\xb8',(name,'missing type-hash immediate')
                # LLVM seals direct-only functions by omitting landing pads.
                # Relocated function pointers and explicit task/entry targets
                # remain legitimate indirect destinations and require pads.
                if symbols[name] in relocation_targets or any(part in name for part in ['worker_task','kernel_run','shell_task','reaper_task']):
                    assert raw(symbols[name],4)==bytes.fromhex('f30f1efa'),(name,'indirect target missing ENDBR64')
            assert sum(op=='endbr64' for op,_ in instructions.values())>50,'missing compiler landing pads'
            entries=[name for name in symbols if name.startswith('entry_')]+['timer_handler','yield_handler','syscall_int_entry','x86_exception_common','user_copy_bytes','protection_test_gate','ap_tramp_lm64']
            for name in entries:assert raw(symbols[name],4)==bytes.fromhex('f30f1efa'),(name,'ASM entry lacks ENDBR64')
            assert 'notrack' not in dump,'compiler emitted NO_TRACK bypass'
            start,end=symbols['__kcfi_traps_start'],symbols['__kcfi_traps_end'];assert (end-start)%4==0 and end-start>100
            for address in range(start,end,4):
                pc=address+struct.unpack('<i',raw(address,4))[0]
                assert raw(pc,2)==b'\x0f\x0b',(hex(pc),'trap record is not UD2')
                assert raw(pc-2,2)==b'\x74\x02',(hex(pc),'missing type-check branch')
            count=(end-start)//4
        else:
            # LLVM marks each ARM type hash as literal data immediately
            # before the function. Exclude the Linux Image header boundary.
            functions=[(name,addr) for name,addr in symbols.items() if name.startswith('_R') and instructions.get(addr-4,('',''))[0]=='.word']
            assert len(functions)>300,'missing ARM type hashes'
            for name,address in functions:
                if address in relocation_targets or any(part in name for part in ['worker_task','kernel_run','shell_task','reaper_task']):
                    # PACIASP is an implicit BTI c landing instruction; BT1
                    # preserves call compatibility while rejecting jump BTYPE.
                    assert instructions[address][0] in ['bti','paciasp'],(name,'indirect target lacks BTI landing pad')
            assert sum(op=='bti' for op,_ in instructions.values())>50,'missing compiler landing pads'
            for name in ['exception_common','user_copy_from','user_copy_to','arm64_stack_overflow','arm64_enter_high','secondary_start','secondary_spin_entry','pan_read_probe','pan_write_probe','wx_write','wx_execute','wx_text']:
                assert raw(symbols[name],4)==bytes.fromhex('df2403d5'),(name,'ASM entry lacks BTI jc')
            traps=[(address,args) for address,(op,args) in instructions.items() if op=='brk' and re.search(r'#0x822[0-9a-f]|#0x823[0-9a-f]',args)]
            assert len(traps)>50,'missing ARM KCFI call checks'
            for address,_ in traps:assert instructions[address-4][0]=='b.eq',(hex(address),'missing type-check branch')
            count=len(traps)
        print(f'{target}: compiler type prefixes, {count} KCFI traps and architectural landing pads verified')
if __name__=='__main__':main()
