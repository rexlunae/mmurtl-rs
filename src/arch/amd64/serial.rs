//! amd64 console UART: 16550 at COM1 (0x3F8).

use uart_16550::SerialPort;

static mut PORT: Option<SerialPort> = None;

/// Initialize COM1 at 115200 8N1. Called once, before any output.
pub fn init() {
    let mut port = unsafe { SerialPort::new(0x3F8) };
    port.init();
    unsafe { *core::ptr::addr_of_mut!(PORT) = Some(port) };
}

/// Transmit one byte. Callers serialize (see `crate::serial`).
pub fn putc(byte: u8) {
    unsafe {
        if let Some(port) = (*core::ptr::addr_of_mut!(PORT)).as_mut() {
            port.send(byte);
        }
    }
}

/// Human-readable name for the boot log
pub const NAME: &str = "COM1 (16550) at 115200 8N1";
