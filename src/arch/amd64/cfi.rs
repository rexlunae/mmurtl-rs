pub fn initialize()->bool {crate::security::common().features & crate::security::feature::IBT!=0}
pub fn status(active:bool)->&'static str {if active {"[CFI] CET-IBT selected; strict supervisor control applied per CPU on entry"} else {"[CFI] CET-IBT unavailable on common CPU set; KCFI remains active"}}
pub fn enable_current() {
    use crate::security::cfi_policy::{CR4_CET,SUPERVISOR_ENDBR};
    unsafe {
        use x86_64::registers::model_specific::Msr;
        let cr0:u64;let cr4:u64;
        core::arch::asm!("mov {}, cr0",out(reg) cr0,options(nomem,nostack));
        assert!(cr0&(1<<16)!=0,"CET requires CR0.WP");
        Msr::new(0x6a0).write(0); // no user CET or shadow-stack ABI yet
        Msr::new(0x6a2).write(0); // initialize supervisor state, SH_STK_EN=0
        core::arch::asm!("mov {}, cr4",out(reg) cr4,options(nomem,nostack));
        core::arch::asm!("mov cr4, {}",in(reg) cr4|CR4_CET,options(nostack));
        Msr::new(0x6a2).write(SUPERVISOR_ENDBR); // ENDBR_EN only; NO_TRACK/legacy suppression disabled
        assert_eq!(Msr::new(0x6a2).read(),SUPERVISOR_ENDBR);
        let actual:u64;core::arch::asm!("mov {}, cr4",out(reg) actual,options(nomem,nostack));
        assert_ne!(actual&CR4_CET,0);
    }
}
#[cfg(any(feature="test-bti",feature="test-ibt"))]
core::arch::global_asm!(r#"
.text
.global cfi_landing_probe
cfi_landing_probe:
    endbr64
    lea rax, [rip + cfi_non_landing]
    jmp rax
    int3
.global cfi_non_landing
cfi_non_landing:
    nop
    jmp kernel_cfi_probe_escaped
"#);
#[cfg(any(feature="test-bti",feature="test-ibt"))]
pub unsafe fn test_landing()->! {
    unsafe extern "C" {fn cfi_landing_probe()->!;}
    cfi_landing_probe()
}
pub fn is_kcfi_trap(pc:u64) -> bool {
    unsafe extern "C" { static __kcfi_traps_start:u8;static __kcfi_traps_end:u8; }
    let start=core::ptr::addr_of!(__kcfi_traps_start) as u64;
    let end=core::ptr::addr_of!(__kcfi_traps_end) as u64;
    for address in (start..end).step_by(4) {
        let offset=unsafe {(address as *const i32).read_unaligned()} as i64;
        if address.wrapping_add_signed(offset)==pc {return true;}
    }
    false
}
/// Decode only the compiler's bounded ADD/type-check sequence immediately
/// before a whitelisted trap. Unknown encodings retain a fatal no-target log.
pub fn kcfi_target(frame:&x86_64::structures::idt::InterruptStackFrame) -> Option<u64> {
    let pc=frame.instruction_pointer.as_u64();
    let mut register=None;
    for length in [6usize,7] {
        let bytes=unsafe {core::slice::from_raw_parts((pc-length as u64) as *const u8,length)};
        if !matches!(bytes[0],0x44|0x45) || bytes[1]!=0x03 || bytes[length-2..]!=[0x74,0x02] {continue;}
        if bytes[2]&0xf8!=0x50 {continue;}
        let mut base=bytes[2]&7;
        if length==6 && base!=4 && bytes[3]==0xfc {}
        else if length==7 && base==4 && bytes[3]==0x24 && bytes[4]==0xfc {}
        else {continue;}
        if bytes[0]&1!=0 {base+=8;}
        register=Some(base as usize);break;
    }
    let register=register?;
    if register==4 {return Some(frame.stack_pointer.as_u64());}
    let offsets=[14,13,12,11,0,10,9,8,7,6,5,4,3,2,1,0];
    // Normalized exception entry saves 15 GPRs and vector/error before the
    // borrowed five-word CPU frame. See x86_exception_common.
    let saved=(frame as *const _ as *const u8).wrapping_sub(136).cast::<u64>();
    Some(unsafe {saved.add(offsets[register]).read_unaligned()})
}
