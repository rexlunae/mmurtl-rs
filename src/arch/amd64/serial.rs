//! amd64 console UART: 16550 COM1, interrupt-backed receive + poll fallback.

use core::sync::atomic::{AtomicBool, Ordering};
use x86_64::instructions::port::Port;

static RX_LOCK: spin::Mutex<()> = spin::Mutex::new(());
static RX_ERROR: AtomicBool = AtomicBool::new(false);

pub fn init() {
    let mut port = unsafe { uart_16550::SerialPort::new(0x3f8) };
    port.init();
}

// LSR reads clear error bits. Both transmit and receive must preserve
// those errors until the RX path flushes the damaged input stream.
fn status() -> u8 {
    let bits = unsafe { Port::<u8>::new(0x3fd).read() };
    if bits & 0x1e != 0 {
        RX_ERROR.store(true, Ordering::Release);
    }
    bits
}

/// Raw transmit: the CLI itself supplies terminal backspace sequences.
pub fn putc(byte: u8) {
    while status() & 0x20 == 0 {
        core::hint::spin_loop();
    }
    unsafe {
        Port::<u8>::new(0x3f8).write(byte);
    }
}

/// Call once after the IDT and IRQ4 routing are installed.
pub fn enable_rx_irq() {
    unsafe {
        Port::<u8>::new(0x3fa).write(0x01); // FIFO, one-byte RX threshold
        Port::<u8>::new(0x3f9).write(0x05); // RX data + line-status IRQs
    }
}

pub fn handle_rx_irq() {
    let Some(_guard) = RX_LOCK.try_lock() else {
        return;
    };
    loop {
        let bits = status();
        if RX_ERROR.swap(false, Ordering::AcqRel) {
            // Flush all older FIFO bytes so an earlier newline cannot
            // terminate recovery for a later, truncated command.
            unsafe {
                Port::<u8>::new(0x3fa).write(0x03);
            }
            crate::keyboard::input_lost();
            return;
        }
        if bits & 1 == 0 {
            break;
        }
        crate::keyboard::push_char(unsafe { Port::<u8>::new(0x3f8).read() });
    }
}
pub fn poll_input() {
    crate::arch::without_interrupts(handle_rx_irq);
}

pub const NAME: &str = "COM1 (16550) at 115200 8N1";
