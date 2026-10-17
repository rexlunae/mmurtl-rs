//! Architectural forward-edge control-flow integrity and fatal diagnostics.
pub fn fatal(kind: &'static [u8], pc: u64, target: Option<u64>) -> ! {
    #[cfg(target_arch="x86_64")]
    unsafe { core::arch::asm!("cli",options(nomem,nostack)); }
    #[cfg(target_arch="aarch64")]
    unsafe { core::arch::asm!("msr daifset, #15",options(nomem,nostack)); }
    fn write(bytes:&[u8]) { for byte in bytes { crate::arch::stacks::emergency_byte(*byte); } }
    fn address(value:u64) {
        if !crate::kptr::RAW_ADDRESSES { write(b"(ptr)");return; }
        write(b"0x");
        for shift in (0..16).rev() { crate::arch::stacks::emergency_byte(b"0123456789abcdef"[((value>>(shift*4))&15) as usize]); }
    }
    write(b"\n[CFI] fatal ");write(kind);write(b" at ");address(pc);write(b" target ");
    if let Some(target)=target {address(target);} else {write(b"(unknown)");}
    write(b"; CPU halted\n");
    loop { crate::arch::halt(); }
}
#[cfg(feature="test-cfi")]
#[no_mangle]
#[inline(never)]
extern "C" fn cfi_wrong_type_target() -> u64 {
    for byte in b"[CFI TEST] ERROR: wrong-type target entered\n" {crate::arch::stacks::emergency_byte(*byte);}
    0
}
#[cfg(feature="test-cfi")]
pub fn test_mismatch() -> ! {
    let address=core::hint::black_box(cfi_wrong_type_target as *const () as usize);
    let wrong:extern "C" fn(u64)->u64=unsafe {core::mem::transmute(address)};
    core::hint::black_box(wrong(7));
    panic!("KCFI mismatch escaped compiler check");
}
