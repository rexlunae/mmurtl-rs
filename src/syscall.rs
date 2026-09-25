//! Syscalls — the ring-3 → kernel interface, via `int 0x80`.
//!
//! Calling convention (Linux-like register assignment):
//!   RAX = syscall number
//!   RDI = arg1, RSI = arg2, RDX = arg3, R10 = arg4, R8 = arg5
//!   Return value in RAX; every other register is preserved.
//!
//! `int 0x80` is an interrupt gate with DPL 3, so user code may invoke it.
//! On entry from ring 3 the CPU switches to TSS.RSP0 — the calling task's
//! own kernel stack — so a syscall can block (IPC, sleep) and be preempted
//! exactly like kernel code, then IRETQ back to ring 3. (The `syscall`
//! instruction is deliberately left disabled: it does not switch stacks,
//! and a ring-3 caller would otherwise run kernel code on its user stack.)
//!
//! Every pointer argument is validated against the user window and the
//! page tables (`memory::user::range_ok`) before the kernel touches it, so
//! ring 3 can't make the kernel read or write kernel memory for it.

use crate::memory::user::{copy_from_user, copy_to_user, range_ok};
use crate::scheduler::{self, Rqb, RqbStatus, TaskContext, RQB_DATA_SIZE};

/// Interrupt vector for syscalls
pub const SYSCALL_VECTOR: u8 = 0x80;

// ========================================================================
// Syscall numbers
// ========================================================================

/// exit() — terminate the calling task
pub const SYS_EXIT: u64 = 0;
/// log(ptr, len) — write one line to the console
pub const SYS_LOG: u64 = 1;
/// send_rqb(tid, rqb*) — send a request, block for the reply; returns status
pub const SYS_SEND_RQB: u64 = 2;
/// receive_rqb(rqb*) — block until a request arrives
pub const SYS_RECEIVE_RQB: u64 = 3;
/// reply_rqb(tid, rqb*) — answer a blocked sender; returns status
pub const SYS_REPLY_RQB: u64 = 4;
/// get_tid() — calling task's ID
pub const SYS_GET_TID: u64 = 5;
/// sleep_ms(ms)
pub const SYS_SLEEP_MS: u64 = 6;
/// lookup_service(name*, len) — task ID serving `name`, or 0
pub const SYS_LOOKUP_SERVICE: u64 = 7;
/// register_service(name*, len) — serve `name` as the calling task
pub const SYS_REGISTER_SERVICE: u64 = 8;
/// yield()
pub const SYS_YIELD: u64 = 9;

/// Error return: bad pointer
pub const EFAULT: u64 = u64::MAX;
/// Error return: unknown syscall
pub const ENOSYS: u64 = u64::MAX - 1;
/// Error return: bad argument
pub const EINVAL: u64 = u64::MAX - 2;

// ========================================================================
// RQB user ABI — an explicit 96-byte wire layout, independent of Rust's
// struct layout (and never exposing uninitialized padding to ring 3)
// ========================================================================

/// Size of an RQB in user memory
pub const RQB_USER_SIZE: usize = 96;
const OFF_SERVICE: usize = 0;
const OFF_STATUS: usize = 2;
const OFF_SENDER: usize = 4;
const OFF_RECEIVER: usize = 8;
const OFF_DATA_SIZE: usize = 12;
const OFF_DATA: usize = 18;

fn rqb_decode(b: &[u8; RQB_USER_SIZE]) -> Rqb {
    let mut r = Rqb::new();
    r.service = u16::from_le_bytes([b[OFF_SERVICE], b[OFF_SERVICE + 1]]);
    r.status = u16::from_le_bytes([b[OFF_STATUS], b[OFF_STATUS + 1]]);
    r.data_size = u16::from_le_bytes([b[OFF_DATA_SIZE], b[OFF_DATA_SIZE + 1]]).min(RQB_DATA_SIZE as u16);
    r.data.copy_from_slice(&b[OFF_DATA..OFF_DATA + RQB_DATA_SIZE]);
    // sender/receiver are kernel-assigned; user-supplied values are ignored
    r
}

fn rqb_encode(r: &Rqb) -> [u8; RQB_USER_SIZE] {
    let mut b = [0u8; RQB_USER_SIZE];
    b[OFF_SERVICE..OFF_SERVICE + 2].copy_from_slice(&r.service.to_le_bytes());
    b[OFF_STATUS..OFF_STATUS + 2].copy_from_slice(&r.status.to_le_bytes());
    b[OFF_SENDER..OFF_SENDER + 4].copy_from_slice(&r.sender_id.to_le_bytes());
    b[OFF_RECEIVER..OFF_RECEIVER + 4].copy_from_slice(&r.receiver_id.to_le_bytes());
    b[OFF_DATA_SIZE..OFF_DATA_SIZE + 2].copy_from_slice(&r.data_size.to_le_bytes());
    b[OFF_DATA..OFF_DATA + RQB_DATA_SIZE].copy_from_slice(&r.data);
    b
}

fn read_user_rqb(ptr: u64) -> Option<Rqb> {
    let mut b = [0u8; RQB_USER_SIZE];
    copy_from_user(&mut b, ptr).then(|| rqb_decode(&b))
}

// ========================================================================
// Entry stub
// ========================================================================

core::arch::global_asm!(
    ".globl syscall_int_entry",
    "syscall_int_entry:",
    // Same register frame as the timer path (TaskContext layout)
    "push rax",
    "push rcx",
    "push rdx",
    "push rbx",
    "push rbp",
    "push rsi",
    "push rdi",
    "push r8",
    "push r9",
    "push r10",
    "push r11",
    "push r12",
    "push r13",
    "push r14",
    "push r15",
    "mov rdi, rsp",
    // We are on the caller's own kernel stack: run the handler with
    // interrupts on, so it can be preempted and can block.
    "sti",
    "call syscall_dispatch",
    "cli",
    "pop r15",
    "pop r14",
    "pop r13",
    "pop r12",
    "pop r11",
    "pop r10",
    "pop r9",
    "pop r8",
    "pop rdi",
    "pop rsi",
    "pop rbp",
    "pop rbx",
    "pop rdx",
    "pop rcx",
    "pop rax", // the handler stored the return value in the saved RAX
    "iretq",
);

extern "C" {
    fn syscall_int_entry();
}

/// Address of the entry stub, for the IDT
pub fn entry_address() -> u64 {
    syscall_int_entry as usize as u64
}

// ========================================================================
// Dispatch
// ========================================================================

#[no_mangle]
extern "C" fn syscall_dispatch(frame: &mut TaskContext) {
    let (a1, a2) = (frame.rdi, frame.rsi);
    let cpl = frame.cs & 3;
    frame.rax = match frame.rax {
        SYS_EXIT => sys_exit(),
        SYS_LOG => sys_log(a1, a2, cpl),
        SYS_SEND_RQB => sys_send_rqb(a1, a2),
        SYS_RECEIVE_RQB => sys_receive_rqb(a1),
        SYS_REPLY_RQB => sys_reply_rqb(a1, a2),
        SYS_GET_TID => scheduler::current_task_id() as u64,
        SYS_SLEEP_MS => {
            scheduler::sleep_ms(a1.min(3_600_000));
            0
        }
        SYS_LOOKUP_SERVICE => sys_lookup_service(a1, a2),
        SYS_REGISTER_SERVICE => sys_register_service(a1, a2),
        SYS_YIELD => {
            scheduler::yield_now();
            0
        }
        _ => ENOSYS,
    };
}

fn sys_exit() -> u64 {
    let mut line: heapless::String<96> = heapless::String::new();
    use core::fmt::Write;
    let _ = write!(
        line,
        "[USER] T{} \"{}\" exited\n",
        scheduler::current_task_id(),
        scheduler::current_task_name()
    );
    crate::serial::write_str(&line);
    scheduler::exit_current();
}

/// Print one line, prefixed with the caller's task ID and privilege level
fn sys_log(ptr: u64, len: u64, cpl: u64) -> u64 {
    const MAX: usize = 200;
    if len as usize > MAX {
        return EINVAL;
    }
    let mut buf = [0u8; MAX];
    let text = &mut buf[..len as usize];
    if !copy_from_user(text, ptr) {
        return EFAULT;
    }

    use core::fmt::Write;
    let mut line: heapless::String<256> = heapless::String::new();
    let _ = write!(line, "[USER T{} CPL{}] ", scheduler::current_task_id(), cpl);
    for &b in text.iter() {
        let c = if b == b'\n' || b.is_ascii_graphic() || b == b' ' { b as char } else { '?' };
        let _ = line.push(c);
    }
    if !line.ends_with('\n') {
        let _ = line.push('\n');
    }
    crate::serial::write_str(&line);
    len
}

fn sys_send_rqb(tid: u64, ptr: u64) -> u64 {
    // Validate the buffer for the reply BEFORE sending, so a bad pointer
    // can't strand a request the receiver has already answered
    if !range_ok(ptr, RQB_USER_SIZE as u64, true) {
        return EFAULT;
    }
    let mut rqb = match read_user_rqb(ptr) {
        Some(r) => r,
        None => return EFAULT,
    };
    let status = scheduler::send_rqb(tid as u32, &mut rqb);
    if !copy_to_user(ptr, &rqb_encode(&rqb)) {
        return EFAULT;
    }
    status as u64
}

fn sys_receive_rqb(ptr: u64) -> u64 {
    // Validate first: once a request is dequeued, we must be able to
    // deliver it (its sender is blocked waiting on us)
    if !range_ok(ptr, RQB_USER_SIZE as u64, true) {
        return EFAULT;
    }
    let req = scheduler::receive_rqb();
    if !copy_to_user(ptr, &rqb_encode(&req)) {
        // Can't hand it to the caller: fail the request rather than
        // leaving its sender blocked forever
        let mut fail = Rqb::new();
        fail.set_status(RqbStatus::GeneralFailure);
        scheduler::reply_rqb(req.sender_id, &fail);
        return EFAULT;
    }
    0
}

fn sys_reply_rqb(tid: u64, ptr: u64) -> u64 {
    match read_user_rqb(ptr) {
        Some(rqb) => scheduler::reply_rqb(tid as u32, &rqb) as u64,
        None => EFAULT,
    }
}

/// Copy a service name in from user memory
fn user_name(ptr: u64, len: u64) -> Option<heapless::String<16>> {
    if len == 0 || len > 16 {
        return None;
    }
    let mut buf = [0u8; 16];
    if !copy_from_user(&mut buf[..len as usize], ptr) {
        return None;
    }
    let s = core::str::from_utf8(&buf[..len as usize]).ok()?;
    let mut out = heapless::String::new();
    out.push_str(s).ok()?;
    Some(out)
}

fn sys_lookup_service(ptr: u64, len: u64) -> u64 {
    match user_name(ptr, len) {
        Some(n) => crate::ipc::lookup_service(&n).unwrap_or(0) as u64,
        None => EFAULT,
    }
}

fn sys_register_service(ptr: u64, len: u64) -> u64 {
    match user_name(ptr, len) {
        Some(n) if crate::ipc::register_service(&n, scheduler::current_task_id()) => 0,
        Some(_) => EINVAL,
        None => EFAULT,
    }
}

/// Report the syscall interface at boot
pub fn init() {
    crate::serial::write_line("[SYSCALL] int 0x80 gate (DPL 3): 10 syscalls, user pointers validated");
}
