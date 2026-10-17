//! One cached SPEC_CTRL owner preserves controls installed by other mitigations.
use core::sync::atomic::{AtomicBool, AtomicU64, Ordering};
use crate::security::feature as F;
static VALID: [AtomicBool; 64] = [const { AtomicBool::new(false) }; 64];
static VALUE: [AtomicU64; 64] = [const { AtomicU64::new(0) }; 64];
pub unsafe fn read_msr(index: u32) -> u64 {
    let (lo, hi): (u32, u32);
    core::arch::asm!("rdmsr", in("ecx") index, out("eax") lo, out("edx") hi, options(nostack));
    (hi as u64) << 32 | lo as u64
}
pub unsafe fn write_msr(index: u32, value: u64) {
    core::arch::asm!("wrmsr", in("ecx") index, in("eax") value as u32, in("edx") (value >> 32) as u32, options(nostack));
}
/// Calling CPU only, with interrupts disabled. No locks/allocations.
pub fn update(clear: u64, set: u64) -> u64 {
    assert!(!x86_64::instructions::interrupts::are_enabled(), "SPEC_CTRL update requires IRQs masked");
    let features = crate::security::common().features;
    let mut allowed = 0;
    for (feature, mask) in [(F::IBRS,1),(F::STIBP,2),(F::SSBD,4),(F::PSFD,1 << 7),
        (F::IPRED_CTRL,(1 << 3)|(1 << 4)),(F::RRSBA_CTRL,(1 << 5)|(1 << 6)),(F::BHI_CTRL,1 << 10)] {
        if features & feature != 0 { allowed |= mask; }
    }
    assert!((clear | set) & !allowed == 0, "unsupported SPEC_CTRL bits");
    if allowed == 0 { return 0; }
    let cpu = super::cpu_index();
    let old = if VALID[cpu].load(Ordering::Acquire) { VALUE[cpu].load(Ordering::Relaxed) }
        else { unsafe { read_msr(0x48) } };
    let new = crate::security::store_bypass::merge_control(old, clear, set, allowed).unwrap();
    if old != new {
        unsafe { write_msr(0x48, new); }
        assert_eq!(unsafe { read_msr(0x48) } & (clear | set), new & (clear | set), "SPEC_CTRL readback failed");
    }
    VALUE[cpu].store(new, Ordering::Relaxed);
    VALID[cpu].store(true, Ordering::Release);
    new
}
