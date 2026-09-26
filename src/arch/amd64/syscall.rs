//! amd64 syscall entry: the `int 0x80` gate.
//!
//! Calling convention (Linux-like register assignment):
//!   RAX = syscall number
//!   RDI = arg1, RSI = arg2, RDX = arg3, R10 = arg4, R8 = arg5
//!   Return value in RAX; every other register is preserved.
//!
//! `int 0x80` is an interrupt gate with DPL 3, so user code may invoke it.
//! On entry from ring 3 the CPU switches to TSS.RSP0 — the calling task's
//! own kernel stack — then IRETQs back to ring 3. (The `syscall`
//! instruction is deliberately left disabled: it does not switch stacks,
//! and a ring-3 caller would otherwise run kernel code on its user stack.)

use super::context::TaskContext;

/// Interrupt vector for syscalls
pub const SYSCALL_VECTOR: u8 = 0x80;

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

/// Decode the saved registers and run the portable dispatcher
#[no_mangle]
extern "C" fn syscall_dispatch(frame: &mut TaskContext) {
    let privilege = match frame.cs & 3 {
        3 => "CPL3",
        _ => "CPL0",
    };
    frame.rax = crate::syscall::dispatch(
        frame.rax,
        [frame.rdi, frame.rsi, frame.rdx, frame.r10, frame.r8],
        privilege,
    );
}
