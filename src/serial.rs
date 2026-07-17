//! Serial I/O — UART 16550 output for kernel debugging and console.

use spin::Mutex;
use uart_16550::SerialPort;

/// Global serial port instance (COM1)
static SERIAL_PORT: Mutex<Option<SerialPort>> = Mutex::new(None);

/// Initialize serial port at COM1 (0x3F8)
pub fn init() {
    let mut port = unsafe { SerialPort::new(0x3F8) };
    port.init();
    *SERIAL_PORT.lock() = Some(port);

    // Flush any garbage from port init
    write_str("[SERIAL] COM1 initialized at 115200 8N1\n");
}

/// Write a string to the serial console
pub fn write_str(s: &str) {
    let mut guard = SERIAL_PORT.lock();
    if let Some(port) = guard.as_mut() {
        for byte in s.bytes() {
            // Filter carriage returns, let \n through (serial handles LF)
            if byte != b'\r' {
                port.send(byte);
            }
        }
    }
}

/// Write a decimal number to serial
pub fn write_dec(value: u64) {
    if value == 0 {
        write_str("0");
        return;
    }
    let mut buf = [0u8; 20];
    let mut i = 0;
    let mut n = value;
    while n > 0 {
        buf[i] = b'0' + (n % 10) as u8;
        n /= 10;
        i += 1;
    }
    for j in (0..i).rev() {
        write_str(core::str::from_utf8(&buf[j..j+1]).unwrap());
    }
}

/// Write a hex number to serial
pub fn write_hex(value: u64) {
    // Write hex digits (no 0x prefix — caller adds it)
    for i in (0..16).rev() {
        let digit = (value >> (i * 4)) & 0xF;
        let c = if digit < 10 {
            b'0' + digit as u8
        } else {
            b'a' + (digit - 10) as u8
        };
        write_str(core::str::from_utf8(&[c]).unwrap());
    }
}

/// Write a hex byte
pub fn write_hex_byte(value: u8) {
    let hi = value >> 4;
    let lo = value & 0xF;
    let chars = [
        if hi < 10 { b'0' + hi } else { b'a' + hi - 10 },
        if lo < 10 { b'0' + lo } else { b'a' + lo - 10 },
    ];
    write_str(core::str::from_utf8(&chars).unwrap());
}

/// Write a line to serial
pub fn write_line(s: &str) {
    write_str(s);
    write_str("\n");
}
