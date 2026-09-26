//! Raspberry Pi 2/3 interrupt controllers (no GIC on these boards).
//!
//! - BCM2836 "local" controller (ARM_LOCAL, 0x4000_0000 on the Pi 3): per
//!   core routing of the generic timers and four mailboxes. We use the
//!   virtual timer (CNTV) as the scheduler tick and mailbox 0 as the
//!   reschedule IPI.
//! - BCM2835 "armctrl" controller: the VideoCore peripherals' interrupts
//!   (the PL011 is bank 2 bit 25, i.e. armctrl IRQ 57), reaching core 0
//!   through the local controller's "GPU interrupt" source.
//!
//! IPIs target the core number (MPIDR Aff0).

use core::arch::asm;
use core::sync::atomic::{AtomicU32, AtomicU64, Ordering};

static LOCAL: AtomicU64 = AtomicU64::new(0);
static ARMCTRL: AtomicU64 = AtomicU64::new(0);
/// armctrl interrupt number of the UART (bank * 32 + bit; banks 1 and 2
/// are the GPU pending registers)
static UART_IRQ: AtomicU32 = AtomicU32::new(89);

// Local controller, indexed by core
const TIMER_CTRL: u64 = 0x40; // + 4 * core
const MBOX_CTRL: u64 = 0x50; // + 4 * core
const IRQ_SOURCE: u64 = 0x60; // + 4 * core
const MBOX0_SET: u64 = 0x80; // + 16 * core
const MBOX0_CLR: u64 = 0xC0; // + 16 * core
const TIMER_CNTV: u32 = 1 << 3;
const SRC_CNTV: u32 = 1 << 3;
const SRC_MBOX0: u32 = 1 << 4;
const SRC_GPU: u32 = 1 << 8;

// armctrl: pending / enable registers for banks 1 and 2 (bank 0 is the
// "basic" register; the UART lives in bank 2)
const PENDING1: u64 = 0x04;
const ENABLE1: u64 = 0x10;

fn rd32(addr: u64) -> u32 {
    unsafe { core::ptr::read_volatile(addr as *const u32) }
}
fn wr32(addr: u64, v: u32) {
    unsafe { core::ptr::write_volatile(addr as *mut u32, v) }
}

fn core_id() -> u64 {
    let v: u64;
    unsafe { asm!("mrs {}, mpidr_el1", out(reg) v) };
    v & 0xFF
}

/// Boot CPU: remember the controllers, route the UART, set up this core
pub fn init(local: u64, armctrl: u64, uart_irq: u32) {
    LOCAL.store(local, Ordering::Relaxed);
    ARMCTRL.store(armctrl, Ordering::Relaxed);
    UART_IRQ.store(uart_irq, Ordering::Relaxed);

    // armctrl bank 1/2 enable registers are write-1-to-set; GPU
    // interrupts go to core 0 by default (GPU routing register = 0)
    if (32..96).contains(&uart_irq) {
        let bank = (uart_irq / 32 - 1) as u64;
        wr32(armctrl + ENABLE1 + 4 * bank, 1 << (uart_irq % 32));
    }
    init_cpu();
}

/// Per-core: virtual-timer and mailbox 0 interrupts to IRQ
pub fn init_cpu() {
    let l = LOCAL.load(Ordering::Relaxed);
    let c = core_id();
    wr32(l + TIMER_CTRL + 4 * c, TIMER_CNTV);
    wr32(l + MBOX0_CLR + 16 * c, !0);
    wr32(l + MBOX_CTRL + 4 * c, 1);
}

pub fn cpu_target_id() -> u32 {
    core_id() as u32
}

pub fn send_resched(target: u32) {
    unsafe { asm!("dsb ish") }; // publish scheduler state before the IPI
    wr32(LOCAL.load(Ordering::Relaxed) + MBOX0_SET + 16 * target as u64, 1);
}

/// IRQ dispatch: returns the frame to resume
///
/// # Safety
/// Only from `arm64_exception`, with `sp` the saved frame.
pub unsafe fn handle_irq(sp: u64) -> u64 {
    let l = LOCAL.load(Ordering::Relaxed);
    let c = core_id();
    let src = rd32(l + IRQ_SOURCE + 4 * c);

    if src & SRC_GPU != 0 {
        let u = UART_IRQ.load(Ordering::Relaxed);
        if (32..96).contains(&u) {
            let bank = (u / 32 - 1) as u64;
            if rd32(ARMCTRL.load(Ordering::Relaxed) + PENDING1 + 4 * bank) & (1 << (u % 32)) != 0 {
                super::serial::handle_rx_irq(); // level-triggered: cleared at the UART
            }
        }
    }
    let mut resched = None;
    if src & SRC_MBOX0 != 0 {
        wr32(l + MBOX0_CLR + 16 * c, !0);
        resched = Some(false);
    }
    if src & SRC_CNTV != 0 {
        super::timer::rearm();
        resched = Some(true);
    }
    match resched {
        Some(tick) => crate::scheduler::switch_from_interrupt(sp, tick),
        None => sp,
    }
}
