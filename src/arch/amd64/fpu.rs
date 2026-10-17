//! No FP/SIMD state is switched yet: deny access explicitly on every CPU.
//! This is an isolation policy, not lazy FPU switching. Enabling FP later
//! requires eager save/restore and clean per-task state (issue #41).

pub fn init() {
    unsafe {
        let mut cr0: u64;
        let mut cr4: u64;
        core::arch::asm!("mov {}, cr0", out(reg) cr0, options(nomem, nostack));
        core::arch::asm!("mov {}, cr4", out(reg) cr4, options(nomem, nostack));
        cr0 |= (1 << 1) | (1 << 2) | (1 << 3); // MP, EM, TS (includes FWAIT)
        cr4 &= !((1 << 9) | (1 << 10) | (1 << 18)); // OSFXSR/OSXMMEXCPT/OSXSAVE
        core::arch::asm!("mov cr0, {}", in(reg) cr0, options(nostack));
        core::arch::asm!("mov cr4, {}", in(reg) cr4, options(nostack));
    }
}
