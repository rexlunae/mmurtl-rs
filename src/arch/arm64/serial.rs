//! arm64 console UART: ARM PL011 (address from the device tree; QEMU
//! `virt` puts it at 0x0900_0000). Transmit is polled; receive is
//! interrupt-driven and feeds the console input queue (`crate::keyboard`),
//! so the same echo task works on both ports.

use core::sync::atomic::{AtomicU64, Ordering};

const DR: u64 = 0x00; // data
const FR: u64 = 0x18; // flags
const IMSC: u64 = 0x38; // interrupt mask set/clear
const ICR: u64 = 0x44; // interrupt clear
const FR_RXFE: u32 = 1 << 4; // receive FIFO empty
const FR_TXFF: u32 = 1 << 5; // transmit FIFO full
const INT_RX: u32 = 1 << 4;
const INT_RT: u32 = 1 << 6; // receive timeout

static BASE: AtomicU64 = AtomicU64::new(0x0900_0000);

fn rd(off: u64) -> u32 {
    unsafe { core::ptr::read_volatile((BASE.load(Ordering::Relaxed) + off) as *const u32) }
}

fn wr(off: u64, v: u32) {
    unsafe { core::ptr::write_volatile((BASE.load(Ordering::Relaxed) + off) as *mut u32, v) }
}

/// Use the PL011 at `base` (call before `crate::serial::init`)
pub fn set_base(base: u64) {
    BASE.store(base, Ordering::Relaxed);
}

/// Initialize the UART. QEMU's PL011 needs no baud setup; firmware on real
/// boards leaves it configured.
pub fn init() {}

/// Transmit one byte. Callers serialize (see `crate::serial`).
pub fn putc(byte: u8) {
    while rd(FR) & FR_TXFF != 0 {
        core::hint::spin_loop();
    }
    wr(DR, byte as u32);
}

/// Enable RX interrupts (after the GIC routes the UART's SPI)
pub fn enable_rx_irq() {
    wr(ICR, 0x7FF);
    wr(IMSC, INT_RX | INT_RT);
}

/// UART interrupt: drain the RX FIFO into the console input queue
pub fn handle_rx_irq() {
    while rd(FR) & FR_RXFE == 0 {
        let c = (rd(DR) & 0xFF) as u8;
        crate::keyboard::push_char(if c == b'\r' { b'\n' } else { c });
    }
    wr(ICR, INT_RX | INT_RT);
}

/// Human-readable name for the boot log
pub const NAME: &str = "PL011 UART";
