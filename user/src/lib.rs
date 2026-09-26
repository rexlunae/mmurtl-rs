//! MMURTL/RS user-program runtime: the syscall ABI, RQB messages, console
//! output, and the program entry point — for both amd64 (`int 0x80`) and
//! arm64 (`svc #0`).
//!
//! A program is `#![no_std] #![no_main]` and names its main function with
//! `mmurtl::entry!(main)`.

#![no_std]

use core::fmt;

// Syscall numbers (see the kernel's src/syscall.rs)
pub const SYS_EXIT: u64 = 0;
pub const SYS_LOG: u64 = 1;
pub const SYS_SEND_RQB: u64 = 2;
pub const SYS_RECEIVE_RQB: u64 = 3;
pub const SYS_REPLY_RQB: u64 = 4;
pub const SYS_GET_TID: u64 = 5;
pub const SYS_SLEEP_MS: u64 = 6;
pub const SYS_LOOKUP_SERVICE: u64 = 7;
pub const SYS_REGISTER_SERVICE: u64 = 8;
pub const SYS_YIELD: u64 = 9;

/// Raw syscall: number + up to three arguments
#[cfg(target_arch = "x86_64")]
pub unsafe fn syscall(n: u64, a1: u64, a2: u64, a3: u64) -> u64 {
    let r;
    core::arch::asm!("int 0x80", inlateout("rax") n => r, in("rdi") a1, in("rsi") a2, in("rdx") a3,
        options(nostack));
    r
}

/// Raw syscall: number + up to three arguments
#[cfg(target_arch = "aarch64")]
pub unsafe fn syscall(n: u64, a1: u64, a2: u64, a3: u64) -> u64 {
    let r;
    core::arch::asm!("svc #0", in("x8") n, inlateout("x0") a1 => r, in("x1") a2, in("x2") a3,
        options(nostack));
    r
}

/// Error returns are the top few u64 values
pub fn is_err(r: u64) -> bool {
    r >= u64::MAX - 15
}

pub fn exit() -> ! {
    unsafe { syscall(SYS_EXIT, 0, 0, 0) };
    loop {}
}

/// Write one line to the kernel console
pub fn log(s: &str) {
    unsafe { syscall(SYS_LOG, s.as_ptr() as u64, s.len() as u64, 0) };
}

pub fn tid() -> u32 {
    unsafe { syscall(SYS_GET_TID, 0, 0, 0) as u32 }
}

pub fn sleep_ms(ms: u64) {
    unsafe { syscall(SYS_SLEEP_MS, ms, 0, 0) };
}

pub fn lookup_service(name: &str) -> Option<u32> {
    let r = unsafe { syscall(SYS_LOOKUP_SERVICE, name.as_ptr() as u64, name.len() as u64, 0) };
    (r != 0 && !is_err(r)).then_some(r as u32)
}

/// The kernel's RQB wire format (96 bytes)
#[repr(C)]
pub struct Rqb {
    pub service: u16,
    pub status: u16,
    pub sender: u32,
    pub receiver: u32,
    pub data_size: u16,
    reserved: [u8; 4],
    pub data: [u8; 64],
    pad: [u8; 14],
}

const _: () = assert!(core::mem::size_of::<Rqb>() == 96);

impl Rqb {
    pub fn new(service: u16) -> Self {
        Self { service, status: 0, sender: 0, receiver: 0, data_size: 0, reserved: [0; 4], data: [0; 64], pad: [0; 14] }
    }
    pub fn payload(&self) -> &[u8] {
        &self.data[..(self.data_size as usize).min(64)]
    }
    pub fn set_payload(&mut self, bytes: &[u8]) {
        let n = bytes.len().min(64);
        self.data[..n].copy_from_slice(&bytes[..n]);
        self.data_size = n as u16;
    }
}

/// Send a request and block for the reply; returns the reply status
pub fn send_rqb(to: u32, rqb: &mut Rqb) -> u64 {
    unsafe { syscall(SYS_SEND_RQB, to as u64, rqb as *mut Rqb as u64, 0) }
}

/// Fixed-capacity line buffer for formatted output
pub struct Line {
    buf: [u8; 192],
    len: usize,
}

impl Line {
    pub const fn new() -> Self {
        Self { buf: [0; 192], len: 0 }
    }
    pub fn as_str(&self) -> &str {
        core::str::from_utf8(&self.buf[..self.len]).unwrap_or("?")
    }
}

impl fmt::Write for Line {
    fn write_str(&mut self, s: &str) -> fmt::Result {
        let n = s.len().min(self.buf.len() - self.len);
        self.buf[self.len..self.len + n].copy_from_slice(&s.as_bytes()[..n]);
        self.len += n;
        Ok(())
    }
}

/// `println!` to the kernel console (one line per call)
#[macro_export]
macro_rules! println {
    ($($arg:tt)*) => {{
        use core::fmt::Write;
        let mut line = $crate::Line::new();
        let _ = write!(line, $($arg)*);
        $crate::log(line.as_str());
    }};
}

/// Declare the program's main function
#[macro_export]
macro_rules! entry {
    ($main:path) => {
        #[no_mangle]
        #[link_section = ".text._start"]
        pub extern "C" fn _start() -> ! {
            $main();
            $crate::exit()
        }
    };
}

#[panic_handler]
fn panic(info: &core::panic::PanicInfo) -> ! {
    use core::fmt::Write;
    let mut line = Line::new();
    let _ = write!(line, "user panic: {}", info.message());
    log(line.as_str());
    exit()
}
