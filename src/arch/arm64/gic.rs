//! GICv2 interrupt controller + the ARM generic timer.
//!
//! Interrupts used:
//!   - SGI 1: reschedule IPI (like amd64's vector 0x30)
//!   - PPI (virtual timer, INTID 27 on QEMU virt): the per-CPU scheduler tick
//!   - SPI (PL011 UART RX): console input, routed to CPU 0
//!
//! Addresses and INTIDs come from the device tree.

use core::arch::asm;
use core::sync::atomic::{AtomicU32, AtomicU64, Ordering};

// Distributor registers
const GICD_CTLR: u64 = 0x000;
const GICD_ISENABLER: u64 = 0x100;
const GICD_IPRIORITYR: u64 = 0x400;
const GICD_ITARGETSR: u64 = 0x800;
const GICD_ICFGR: u64 = 0xC00;
const GICD_SGIR: u64 = 0xF00;
// CPU interface registers
const GICC_CTLR: u64 = 0x000;
const GICC_PMR: u64 = 0x004;
const GICC_BPR: u64 = 0x008;
const GICC_IAR: u64 = 0x00C;
const GICC_EOIR: u64 = 0x010;

/// SGI used for reschedule IPIs
pub const RESCHED_SGI: u32 = 1;

static GICD: AtomicU64 = AtomicU64::new(0);
static GICC: AtomicU64 = AtomicU64::new(0);
static TIMER_IRQ: AtomicU32 = AtomicU32::new(27);
static UART_IRQ: AtomicU32 = AtomicU32::new(33);
/// Timer ticks per scheduler tick (CNTFRQ / hz)
static TICK_INTERVAL: AtomicU64 = AtomicU64::new(0);

fn rd(base: &AtomicU64, off: u64) -> u32 {
    unsafe { core::ptr::read_volatile((base.load(Ordering::Relaxed) + off) as *const u32) }
}

fn wr(base: &AtomicU64, off: u64, v: u32) {
    unsafe { core::ptr::write_volatile((base.load(Ordering::Relaxed) + off) as *mut u32, v) }
}

fn wr8(base: &AtomicU64, off: u64, v: u8) {
    unsafe { core::ptr::write_volatile((base.load(Ordering::Relaxed) + off) as *mut u8, v) }
}

fn enable_irq(intid: u32, priority: u8) {
    wr8(&GICD, GICD_IPRIORITYR + intid as u64, priority);
    wr(&GICD, GICD_ISENABLER + 4 * (intid / 32) as u64, 1 << (intid % 32));
}

/// Distributor setup (boot CPU), then this CPU's interface
pub fn init(gicd: u64, gicc: u64, timer_irq: u32, uart_irq: u32) {
    GICD.store(gicd, Ordering::Relaxed);
    GICC.store(gicc, Ordering::Relaxed);
    TIMER_IRQ.store(timer_irq, Ordering::Relaxed);
    UART_IRQ.store(uart_irq, Ordering::Relaxed);

    wr(&GICD, GICD_CTLR, 0);
    // UART RX: level-triggered SPI, delivered to CPU 0
    let u = uart_irq as u64;
    let cfg_off = GICD_ICFGR + 4 * (u / 16);
    let shift = 2 * (u % 16);
    wr(&GICD, cfg_off, rd(&GICD, cfg_off) & !(0b10 << shift));
    wr8(&GICD, GICD_ITARGETSR + u, 1 << cpu_interface_id());
    enable_irq(uart_irq, 0xA0);
    wr(&GICD, GICD_CTLR, 1);

    init_cpu();
}

/// Per-CPU interface setup; also enables this CPU's banked SGI/PPI
pub fn init_cpu() {
    enable_irq(RESCHED_SGI, 0x80);
    enable_irq(TIMER_IRQ.load(Ordering::Relaxed), 0x80);
    wr(&GICC, GICC_PMR, 0xF0);
    wr(&GICC, GICC_BPR, 0);
    wr(&GICC, GICC_CTLR, 1);
}

/// This CPU's GIC CPU interface number (the SGI target bit). The banked
/// GICD_ITARGETSR0 reads back the calling CPU's own mask.
pub fn cpu_interface_id() -> u32 {
    let mask = rd(&GICD, GICD_ITARGETSR) & 0xFF;
    if mask == 0 { 0 } else { mask.trailing_zeros() }
}

/// Send the reschedule SGI to the CPU with interface number `target`
pub fn send_resched(target: u32) {
    unsafe { asm!("dsb ish") }; // publish scheduler state before the IPI
    wr(&GICD, GICD_SGIR, (1 << (16 + target)) | RESCHED_SGI);
}

/// IRQ dispatch from the vector table. Returns the frame to resume.
///
/// # Safety
/// Only from `arm64_exception`, with `sp` the saved frame.
pub unsafe fn handle_irq(sp: u64) -> u64 {
    let iar = rd(&GICC, GICC_IAR);
    let intid = iar & 0x3FF;
    if intid >= 1020 {
        return sp; // spurious
    }
    if intid == TIMER_IRQ.load(Ordering::Relaxed) {
        rearm_timer();
        wr(&GICC, GICC_EOIR, iar);
        return crate::scheduler::switch_from_interrupt(sp, true);
    }
    if intid == RESCHED_SGI {
        wr(&GICC, GICC_EOIR, iar);
        return crate::scheduler::switch_from_interrupt(sp, false);
    }
    if intid == UART_IRQ.load(Ordering::Relaxed) {
        super::serial::handle_rx_irq();
    }
    wr(&GICC, GICC_EOIR, iar);
    sp
}

// ========================================================================
// Generic timer (EL1 virtual timer)
// ========================================================================

fn counter_freq() -> u64 {
    let f: u64;
    unsafe { asm!("mrs {}, cntfrq_el0", out(reg) f) };
    f
}

fn counter() -> u64 {
    let c: u64;
    unsafe { asm!("isb", "mrs {}, cntvct_el0", out(reg) c) };
    c
}

fn rearm_timer() {
    let ticks = TICK_INTERVAL.load(Ordering::Relaxed);
    unsafe { asm!("msr cntv_tval_el0, {}", in(reg) ticks) };
}

/// Start this CPU's periodic tick at `hz`
pub fn start_timer(hz: u32) {
    TICK_INTERVAL.store(counter_freq() / hz as u64, Ordering::Relaxed);
    rearm_timer();
    unsafe { asm!("msr cntv_ctl_el0, {}", "isb", in(reg) 1u64) }; // enable, unmasked
}

/// Busy-wait for `ms` milliseconds on the virtual counter
pub fn delay_ms(ms: u32) {
    let end = counter() + counter_freq() * ms as u64 / 1000;
    while counter() < end {
        core::hint::spin_loop();
    }
}

/// Counter frequency in Hz (for the boot log)
pub fn frequency() -> u64 {
    counter_freq()
}
