//! arm64 console UART: ARM PL011 (address from the device tree; QEMU
//! `virt` puts it at 0x0900_0000). Transmit is polled; receive is
//! interrupt-driven and feeds the console input queue (`crate::keyboard`),
//! so the same echo task works on both ports.

use core::sync::atomic::{AtomicU64, Ordering};

const DR: u64 = 0x00; // data
const FR: u64 = 0x18; // flags
const LCR_H: u64 = 0x2C; // line control
const CR: u64 = 0x30; // control
const IMSC: u64 = 0x38; // interrupt mask set/clear
const ICR: u64 = 0x44; // interrupt clear
const FR_BUSY: u32 = 1 << 3;
const FR_RXFE: u32 = 1 << 4; // receive FIFO empty
const FR_TXFF: u32 = 1 << 5; // transmit FIFO full
const INT_RX: u32 = 1 << 4;
const INT_RT: u32 = 1 << 6; // receive timeout
const LCR_H_FEN: u32 = 1 << 4; // FIFOs enabled
const LCR_H_WLEN8: u32 = 0b11 << 5;
const CR_UARTEN: u32 = 1 << 0;
const CR_TXE: u32 = 1 << 8;
const CR_RXE: u32 = 1 << 9;

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

/// Initialize the UART: keep the baud rate the firmware chose (QEMU needs
/// none), but make sure the FIFOs are on — out of reset the PL011 has a
/// one-character receive holding register, and input typed faster than
/// the interrupt is serviced would be dropped.
pub fn init() {
    let cr = rd(CR);
    wr(CR, 0); // the line control register may only change while disabled
    while rd(FR) & FR_BUSY != 0 {
        core::hint::spin_loop();
    }
    let lcr = rd(LCR_H);
    wr(LCR_H, lcr | LCR_H_FEN | if lcr & LCR_H_WLEN8 == 0 { LCR_H_WLEN8 } else { 0 });
    wr(CR, cr | CR_UARTEN | CR_TXE | CR_RXE);
}

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
