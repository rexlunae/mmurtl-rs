//! ARM generic timer: the EL1 virtual timer drives each CPU's scheduler
//! tick; the virtual counter provides delays.

use core::arch::asm;
use core::sync::atomic::{AtomicU64, Ordering};

/// Counter ticks per scheduler tick (CNTFRQ / hz)
static TICK_INTERVAL: AtomicU64 = AtomicU64::new(0);

/// Counter frequency in Hz
pub fn frequency() -> u64 {
    let f: u64;
    unsafe { asm!("mrs {}, cntfrq_el0", out(reg) f) };
    f
}

fn counter() -> u64 {
    let c: u64;
    unsafe { asm!("isb", "mrs {}, cntvct_el0", out(reg) c) };
    c
}

/// Re-arm this CPU's timer for the next tick
pub fn rearm() {
    let ticks = TICK_INTERVAL.load(Ordering::Relaxed);
    unsafe { asm!("msr cntv_tval_el0, {}", in(reg) ticks) };
}

/// Start this CPU's periodic tick at `hz`
pub fn start(hz: u32) {
    TICK_INTERVAL.store(frequency() / hz as u64, Ordering::Relaxed);
    rearm();
    unsafe { asm!("msr cntv_ctl_el0, {}", "isb", in(reg) 1u64) }; // enable, unmasked
}

/// Busy-wait for `ms` milliseconds on the virtual counter
pub fn delay_ms(ms: u32) {
    let end = counter() + frequency() * ms as u64 / 1000;
    while counter() < end {
        core::hint::spin_loop();
    }
}
