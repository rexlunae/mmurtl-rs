//! Architectural forward-edge control-flow integrity and fatal diagnostics.
use core::sync::atomic::{AtomicBool,AtomicU8,AtomicU64,Ordering};
static ACTIVE:AtomicBool=AtomicBool::new(false);
static CPUS:AtomicU64=AtomicU64::new(0);
static STATE:[AtomicU8;crate::scheduler::MAX_CPUS]=[const {AtomicU8::new(0)};crate::scheduler::MAX_CPUS];
pub fn enabled()->bool {ACTIVE.load(Ordering::Acquire)}
pub fn init() {
    let admitted=crate::security::freeze();
    let active=crate::arch::cfi::initialize();
    CPUS.store(admitted,Ordering::Release);ACTIVE.store(active,Ordering::Release);
    apply_current();
    crate::serial::write_line("[CFI] KCFI compiler type checks active (kernel/core/alloc)");
    crate::serial::write_line(crate::arch::cfi::status(active));
}
pub fn apply_current() {
    if !enabled() {return;}
    crate::arch::without_interrupts(|| {
        let cpu=crate::arch::cpu_index();
        if CPUS.load(Ordering::Acquire)&(1<<cpu)==0 {return;}
        // A nested NMI must never retry an in-progress MSR/control update.
        if STATE[cpu].compare_exchange(0,1,Ordering::AcqRel,Ordering::Acquire).is_err() {return;}
        crate::arch::cfi::enable_current();STATE[cpu].store(2,Ordering::Release);
    });
}
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
#[cfg(any(feature="test-bti",feature="test-ibt"))]
pub fn test_landing() -> ! {
    if !enabled() {
        crate::serial::write_line("[CFI TEST] hardware landing enforcement unavailable; positive test skipped");
        loop {crate::arch::halt();}
    }
    unsafe {crate::arch::cfi::test_landing();}
}
#[cfg(any(feature="test-bti",feature="test-ibt"))]
#[no_mangle]
extern "C" fn kernel_cfi_probe_escaped() -> ! {
    for byte in b"[CFI TEST] ERROR: non-landing target entered\n" {crate::arch::stacks::emergency_byte(*byte);}
    loop {crate::arch::halt();}
}
