//! Syscall — Fast kernel-space syscalls via the `syscall` instruction.
//!
//! Sets up the LSTAR MSR with the assembly trampoline, configures STAR/SF_MASK MSRs,
//! and provides a dispatch table for syscall handlers.
//!
//! Calling convention (matching Linux x86_64):
//!   RAX = syscall number
//!   RDI = arg1, RSI = arg2, RDX = arg3, R10 = arg4, R8 = arg5, R9 = arg6
//!   Return: RAX
//!   Clobbered: RCX, R11 (SYSCALL instruction), all arg registers
//!   Preserved: RBX, RBP, R12–R15 (callee-saved per C ABI)
//!
//! NOTE: All tasks currently run in ring 0. The `syscall` instruction preserves
//! RSP (unlike interrupts), so we push/pop the caller's stack directly.
//! When user-space (ring 3) tasks are introduced, the trampoline will need
//! `swapgs` and a ring-0 stack switch plus `sysretq`/`iretq` return.

// ========================================================================
// Syscall Numbers
// ========================================================================

/// Exit current task
pub const SYS_EXIT: u64 = 0;
/// Write a string to console
pub const SYS_LOG_STR: u64 = 1;
/// Send an RQB message
pub const SYS_SEND_RQB: u64 = 2;
/// Wait for an RQB message
pub const SYS_WAIT_RQB: u64 = 3;
/// Reply to an RQB sender
pub const SYS_REPLY_RQB: u64 = 4;
/// Get current task ID
pub const SYS_GET_TID: u64 = 5;
/// Get system info
pub const SYS_INFO: u64 = 6;

/// Total number of syscalls
pub const SYS_COUNT: u64 = 7;

// ========================================================================
// Syscall Dispatch Table
// ========================================================================

/// A syscall handler: receives (arg1..arg6), returns a value.
type SyscallHandler = fn(u64, u64, u64, u64, u64, u64) -> u64;

/// The dispatch table indexed by syscall number.
/// Entries must be in the same order as SYS_* constants above.
const SYSCALL_HANDLERS: &[Option<SyscallHandler>] = &[
    Some(handle_exit),      // 0
    Some(handle_log_str),   // 1
    Some(handle_send_rqb),  // 2
    Some(handle_wait_rqb),  // 3
    Some(handle_reply_rqb), // 4
    Some(handle_get_tid),   // 5
    Some(handle_info),      // 6
];

// ========================================================================
// Boot-time MSR Setup
// ========================================================================

/// Initialize the MSRs needed for `syscall`.
///
/// Must be called once at boot, after the GDT has been initialized.
///
/// Configures:
///   - `IA32_EFER.SCE` — enable syscall instruction
///   - `IA32_STAR` — ring 0 segment selectors
///   - `IA32_LSTAR` — syscall entry point (this module's trampoline)
///   - `IA32_FMASK` — mask IF (interrupts disabled during handler)
pub fn init() {
    use x86_64::registers::model_specific::{
        Efer, EferFlags, LStar, Star, SFMask,
    };
    use x86_64::registers::rflags::RFlags;
    use x86_64::VirtAddr;

    // 0. Enable SYSCALL in EFER
    let efer = Efer::read();
    if !efer.contains(EferFlags::SYSTEM_CALL_EXTENSIONS) {
        unsafe {
            Efer::write(EferFlags::SYSTEM_CALL_EXTENSIONS);
        }
    }

    // 1. STAR — segment selectors
    //
    //    For SYSCALL (ring 0 entry, 64-bit mode):
    //      CS = STAR[47:32], SS = STAR[47:32] + 8
    //
    //    GDT layout:
    //      0x08 = kernel_code  0x10 = kernel_data
    //      0x18 = user_data    0x20 = user_code
    //      0x28 = TSS
    //
    //    SYSCALL: kernel_code=0x08, kernel_data=0x10
    //      CS = 0x08 ✓, SS = 0x10 ✓
    //    SYSRET field (STAR[63:48]) is also set for future ring-3 support:
    //      user_data=0x18 at base+8 → base=0x10
    //      user_code=0x20 at base+16 → base=0x10 ✓
    unsafe {
        Star::write_raw(0x10, 0x08);
    }

    // 2. LSTAR — syscall entry point
    let entry = VirtAddr::new(syscall_entry as *const () as u64);
    LStar::write(entry);

    // 3. SF_MASK — we don't mask IF because our return path (`jmp rcx`)
    //    doesn't restore RFLAGS. Handlers run with interrupts enabled,
    //    same as any other ring-0 code. The timer can preempt us safely.
    SFMask::write(RFlags::from_bits_truncate(0));

    // 4. Verify
    let star_raw = Star::read_raw();
    let lstar = LStar::read();
    let mask = SFMask::read();
    crate::serial::write_str("[SYSCALL] STAR=0x");
    crate::serial::write_hex(((star_raw.1 as u64) << 32) | (star_raw.0 as u64));
    crate::serial::write_str(" LSTAR=0x");
    crate::serial::write_hex(lstar.as_u64());
    crate::serial::write_str(" SF_MASK=0x");
    crate::serial::write_hex(mask.bits());
    crate::serial::write_str("\n");
    crate::serial::write_str("[SYSCALL] Ready: ");
    crate::serial::write_dec(SYS_COUNT);
    crate::serial::write_str(" handlers (ring-0, ints-off)\n");
}

// ========================================================================
// Assembly Trampoline (`syscall` → dispatcher → `ret`)
// ========================================================================

core::arch::global_asm!(
    ".globl syscall_entry",
    "syscall_entry:",

    // SYSCALL does NOT change RSP (unlike interrupts via TSS).
    // All tasks run in ring 0, so we can push/pop the caller's stack directly.
    // The return RIP is in RCX (set by SYSCALL), RFLAGS in R11.

    // Save C ABI callee-saved registers + return RIP + RFLAGS
    "push rcx",              // return RIP
    "push r11",              // saved RFLAGS
    "push rbx",
    "push rbp",
    "push r12",
    "push r13",
    "push r14",
    "push r15",

    // Build C call: fn(syscall_no, arg1, arg2, arg3, arg4, arg5, arg6)
    // Incoming: rax=no, rdi=a1, rsi=a2, rdx=a3, r10=a4, r8=a5, r9=a6
    // C ABI:    rdi=no, rsi=a1, rdx=a2, rcx=a3, r8=a4, r9=a5, [rsp]=a6
    "push r9",               // arg6 on stack
    "mov r9, r8",            // r9 = arg5
    "mov r8, r10",           // r8 = arg4
    "mov rcx, rdx",          // rcx = arg3
    "mov rdx, rsi",          // rdx = arg2
    "mov rsi, rdi",          // rsi = arg1
    "mov rdi, rax",          // rdi = syscall_no

    "call syscall_dispatcher",

    "add rsp, 8",            // pop arg6 from stack

    // Restore callee-saved registers
    "pop r15",
    "pop r14",
    "pop r13",
    "pop r12",
    "pop rbp",
    "pop rbx",

    // Restore RFLAGS into r11, return RIP into rcx
    "pop r11",               // RFLAGS (discarded for ring-0; no iretq)
    "pop rcx",               // return RIP

    // RAX already has the return value from syscall_dispatcher
    // Just jump back — RCX has the return address (set by SYSCALL)
    // and SYSCALL preserved the stack pointer, so ret works.
    // However, SYSCALL clobbers RCX with RIP, so we need `ret` or `jmp rcx`.
    // `push rcx; ret` or simply `jmp rcx`.
    "jmp rcx"
);

/// Extern so the assembly can reference it
extern "C" {
    fn syscall_entry();
}

// ========================================================================
// Dispatcher — called from assembly with 7 args
// ========================================================================

/// Main syscall dispatcher — routes to the right handler.
///
/// # Safety
/// Called directly from assembly. No safety checks on the calling context.
/// All handlers run with interrupts disabled from SF_MASK.
#[no_mangle]
pub extern "C" fn syscall_dispatcher(
    syscall_no: u64,
    arg1: u64,
    arg2: u64,
    arg3: u64,
    arg4: u64,
    arg5: u64,
    arg6: u64,
) -> u64 {
    // Bounds check
    if syscall_no >= SYS_COUNT {
        crate::serial::write_str("[SYSCALL] ERROR: invalid number ");
        crate::serial::write_dec(syscall_no);
        crate::serial::write_str("\n");
        return !0u64; // -1 on error
    }

    // Look up handler
    match SYSCALL_HANDLERS[syscall_no as usize] {
        Some(handler) => handler(arg1, arg2, arg3, arg4, arg5, arg6),
        None => {
            crate::serial::write_str("[SYSCALL] ERROR: unimplemented syscall ");
            crate::serial::write_dec(syscall_no);
            crate::serial::write_str("\n");
            !0u64
        }
    }
}

// ========================================================================
// Built-in Syscall Handlers
// ========================================================================

/// Exit the current task
fn handle_exit(_: u64, _: u64, _: u64, _: u64, _: u64, _: u64) -> u64 {
    crate::serial::write_str("[SYSCALL] exit\n");
    crate::scheduler::mark_current_exited();
    0
}

/// Write a string to the serial console
fn handle_log_str(ptr: u64, len: u64, _: u64, _: u64, _: u64, _: u64) -> u64 {
    if len == 0 || len > 1024 {
        return !0u64;
    }
    unsafe {
        let slice = core::slice::from_raw_parts(ptr as *const u8, len as usize);
        if let Ok(s) = core::str::from_utf8(slice) {
            crate::serial::write_str(s);
            len
        } else {
            !0u64
        }
    }
}

/// Send an RQB to another task
fn handle_send_rqb(receiver_id: u64, rqb_ptr: u64, _: u64, _: u64, _: u64, _: u64) -> u64 {
    unsafe {
        let rqb = &mut *(rqb_ptr as *mut crate::scheduler::Rqb);
        rqb.sender_id = crate::scheduler::current_task_id();
        crate::scheduler::send_message(receiver_id as u32, rqb);
        crate::scheduler::current_rqb_status() as u64
    }
}

/// Wait for an RQB message
fn handle_wait_rqb(rqb_ptr: u64, _: u64, _: u64, _: u64, _: u64, _: u64) -> u64 {
    unsafe {
        let rqb = &mut *(rqb_ptr as *mut crate::scheduler::Rqb);
        crate::scheduler::receive_message(rqb);
        0
    }
}

/// Reply to a sender
fn handle_reply_rqb(sender_id: u64, rqb_ptr: u64, _: u64, _: u64, _: u64, _: u64) -> u64 {
    unsafe {
        let rqb = &*(rqb_ptr as *const crate::scheduler::Rqb);
        crate::scheduler::reply_message(sender_id as u32, rqb);
        0
    }
}

/// Get the current task ID
fn handle_get_tid(_: u64, _: u64, _: u64, _: u64, _: u64, _: u64) -> u64 {
    crate::scheduler::current_task_id() as u64
}

/// Get system info (version string pointer, etc.)
fn handle_info(_: u64, _: u64, _: u64, _: u64, _: u64, _: u64) -> u64 {
    crate::VERSION.as_ptr() as u64
}

// ========================================================================
// Userspace Callable — `syscall()` instruction wrapper
// ========================================================================

/// Execute a syscall via the `syscall` instruction.
///
/// # Safety
/// Unsafe because the kernel dispatching logic runs with interrupts masked
/// (via SF_MASK). The caller is responsible for passing valid pointers.
///
/// Calling convention per the trampoline:
///   RAX = syscall number
///   RDI = arg1, RSI = arg2, RDX = arg3, R10 = arg4, R8 = arg5, R9 = arg6
///   Returns RAX
pub unsafe fn syscall(
    number: u64,
    arg1: u64,
    arg2: u64,
    arg3: u64,
    arg4: u64,
    arg5: u64,
    arg6: u64,
) -> u64 {
    let result: u64;
    core::arch::asm!(
        "syscall",
        in("rax") number,
        in("rdi") arg1,
        in("rsi") arg2,
        in("rdx") arg3,
        in("r10") arg4,
        in("r8")  arg5,
        in("r9")  arg6,
        lateout("rax") result,
        out("rcx") _,  // clobbered by syscall (set to return RIP)
        out("r11") _,  // clobbered by syscall (set to RFLAGS)
        options(nostack),
    );
    result
}

/// Convenience wrapper: get the current task ID
pub fn sys_get_tid() -> u64 {
    unsafe { syscall(SYS_GET_TID, 0, 0, 0, 0, 0, 0) }
}

/// Convenience wrapper: write a string to the serial console
pub fn sys_log_str(s: &str) {
    unsafe {
        syscall(SYS_LOG_STR, s.as_ptr() as u64, s.len() as u64, 0, 0, 0, 0);
    }
}
