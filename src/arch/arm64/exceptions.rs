//! arm64 exception handling: the EL1 vector table, the register frame
//! (which doubles as the saved task context), and dispatch.
//!
//! Every exception — IRQ, `svc` from EL0 (syscall) or EL1 (yield), fault —
//! saves the full register state on the current stack (SP_EL1; the kernel
//! always runs with SPSel=1) and calls `arm64_exception`, which returns the
//! stack pointer to restore from. For a context switch that is another
//! task's saved frame, and the scheduler lock is still held across the
//! stack switch until `scheduler_unlock` (a no-op when nothing switched).
//!
//! A user task's frame always sits at the top of its kernel stack (SP_EL1
//! is left there by the ERET into EL0), so no per-CPU "kernel stack
//! pointer" register needs updating on a switch — unlike amd64's TSS.RSP0.

use core::arch::{asm, global_asm};

/// Saved registers: x0-x30, SP_EL0, ELR_EL1, SPSR_EL1 (272 bytes)
#[repr(C)]
pub struct TaskContext {
    pub x: [u64; 31],
    pub sp_el0: u64,
    pub elr: u64,
    pub spsr: u64,
}

const _: () = assert!(core::mem::size_of::<TaskContext>() == 272);

/// SPSR for an EL1 (kernel) task: EL1h, interrupts enabled
const SPSR_EL1H: u64 = 0b0101;
/// SPSR for an EL0 (user) task: EL0t, interrupts enabled
const SPSR_EL0T: u64 = 0b0000;

/// Initial frame for a kernel task: ERET lands in `entry` at EL1 with
/// interrupts enabled, SP at `stack_top`
pub fn kernel_context(entry: u64, _stack_top: u64) -> TaskContext {
    TaskContext { x: [0; 31], sp_el0: 0, elr: entry, spsr: SPSR_EL1H }
}

/// Initial frame for a user task: ERET lands in `entry` at EL0 on
/// `user_sp`, with `arg` in x0
pub fn user_context(entry: u64, user_sp: u64, arg: u64, _kstack_top: u64) -> TaskContext {
    let mut x = [0; 31];
    x[0] = arg;
    TaskContext { x, sp_el0: user_sp, elr: entry, spsr: SPSR_EL0T }
}

global_asm!(
    r#"
    .section .text.vectors, "ax"

    .macro VENTRY kind
    .balign 0x80
    sub sp, sp, #272
    stp x0, x1, [sp, #0]
    mov x0, #\kind
    b exception_common
    .endm

    .balign 0x800
    .globl exception_vectors
exception_vectors:
    // Current EL with SP_EL0 (unused: the kernel runs with SPSel=1)
    VENTRY 0
    VENTRY 1
    VENTRY 2
    VENTRY 3
    // Current EL with SP_ELx: kernel code
    VENTRY 4   // sync (svc = yield, else a kernel fault)
    VENTRY 5   // IRQ
    VENTRY 6   // FIQ
    VENTRY 7   // SError
    // Lower EL, AArch64: user code
    VENTRY 8   // sync (svc = syscall, else a user fault)
    VENTRY 9   // IRQ
    VENTRY 10  // FIQ
    VENTRY 11  // SError
    // Lower EL, AArch32 (unsupported)
    VENTRY 12
    VENTRY 13
    VENTRY 14
    VENTRY 15

exception_common:
    stp x2, x3, [sp, #16]
    stp x4, x5, [sp, #32]
    stp x6, x7, [sp, #48]
    stp x8, x9, [sp, #64]
    stp x10, x11, [sp, #80]
    stp x12, x13, [sp, #96]
    stp x14, x15, [sp, #112]
    stp x16, x17, [sp, #128]
    stp x18, x19, [sp, #144]
    stp x20, x21, [sp, #160]
    stp x22, x23, [sp, #176]
    stp x24, x25, [sp, #192]
    stp x26, x27, [sp, #208]
    stp x28, x29, [sp, #224]
    mrs x1, sp_el0
    stp x30, x1, [sp, #240]
    mrs x1, elr_el1
    mrs x2, spsr_el1
    stp x1, x2, [sp, #256]

    mov x1, sp
    bl arm64_exception          // x0 = kind, x1 = frame -> x0 = frame to resume
    mov sp, x0
    bl scheduler_unlock         // releases the lock only if we switched

    ldp x1, x2, [sp, #256]
    msr elr_el1, x1
    msr spsr_el1, x2
    ldp x30, x1, [sp, #240]
    msr sp_el0, x1
    ldp x0, x1, [sp, #0]
    ldp x2, x3, [sp, #16]
    ldp x4, x5, [sp, #32]
    ldp x6, x7, [sp, #48]
    ldp x8, x9, [sp, #64]
    ldp x10, x11, [sp, #80]
    ldp x12, x13, [sp, #96]
    ldp x14, x15, [sp, #112]
    ldp x16, x17, [sp, #128]
    ldp x18, x19, [sp, #144]
    ldp x20, x21, [sp, #160]
    ldp x22, x23, [sp, #176]
    ldp x24, x25, [sp, #192]
    ldp x26, x27, [sp, #208]
    ldp x28, x29, [sp, #224]
    add sp, sp, #272
    eret

    .text
    "#
);

extern "C" {
    static exception_vectors: u8;
}

/// Install the vector table on the calling CPU
pub fn init() {
    unsafe {
        asm!(
            "msr vbar_el1, {v}",
            "isb",
            v = in(reg) core::ptr::addr_of!(exception_vectors) as u64,
        );
    }
}

// ESR_EL1 exception classes
const EC_UNKNOWN: u64 = 0x00;
const EC_FP_ACCESS: u64 = 0x07;
const EC_ILLEGAL_STATE: u64 = 0x0E;
const EC_SVC64: u64 = 0x15;
const EC_SYSREG: u64 = 0x18;
const EC_IABT_LOWER: u64 = 0x20;
const EC_PC_ALIGN: u64 = 0x22;
const EC_DABT_LOWER: u64 = 0x24;
const EC_SP_ALIGN: u64 = 0x26;
const EC_BRK: u64 = 0x3C;

fn esr() -> u64 {
    let v: u64;
    unsafe { asm!("mrs {}, esr_el1", out(reg) v) };
    v
}

fn far() -> u64 {
    let v: u64;
    unsafe { asm!("mrs {}, far_el1", out(reg) v) };
    v
}

/// The Rust side of every exception. Returns the frame to resume.
#[no_mangle]
unsafe extern "C" fn arm64_exception(kind: u64, frame: *mut TaskContext) -> u64 {
    let sp = frame as u64;
    match kind {
        // Kernel sync: `svc` is a voluntary yield; anything else is a bug
        4 => {
            let esr = esr();
            if esr >> 26 == EC_SVC64 {
                return crate::scheduler::yield_and_switch(sp);
            }
            kernel_fault("synchronous exception", esr, &*frame)
        }
        // IRQ from kernel or user code
        5 | 9 => super::gic::handle_irq(sp),
        // User sync: syscall or fault
        8 => {
            let esr = esr();
            let ec = esr >> 26;
            if ec == EC_SVC64 {
                syscall(&mut *frame);
                return sp;
            }
            user_fault(ec, esr, &*frame)
        }
        _ => kernel_fault("unexpected exception", esr(), &*frame),
    }
}

/// `svc #0` from EL0: x8 = number, x0-x4 = arguments, result in x0.
/// Runs on the task's kernel stack with interrupts enabled, so it can
/// block and be preempted like kernel code.
unsafe fn syscall(frame: &mut TaskContext) {
    asm!("msr daifclr, #2");
    let r = crate::syscall::dispatch(
        frame.x[8],
        [frame.x[0], frame.x[1], frame.x[2], frame.x[3], frame.x[4]],
        "EL0",
    );
    asm!("msr daifset, #2");
    frame.x[0] = r;
}

/// A fault in EL0 code kills the task; the kernel carries on
fn user_fault(ec: u64, esr: u64, frame: &TaskContext) -> ! {
    let (what, addr) = match ec {
        EC_DABT_LOWER | EC_IABT_LOWER => {
            let fsc = esr & 0x3F;
            let what = match (ec == EC_DABT_LOWER, fsc >> 2) {
                (true, 0b0011) => "data abort (permission fault)",
                (true, 0b0001) => "data abort (translation fault)",
                (true, _) => "data abort",
                (false, 0b0011) => "instruction abort (permission fault)",
                (false, _) => "instruction abort",
            };
            (what, Some(far()))
        }
        EC_SYSREG => ("trapped system instruction", None),
        EC_UNKNOWN => ("undefined instruction", None),
        EC_FP_ACCESS => ("FP/SIMD access", None),
        EC_PC_ALIGN => ("PC alignment fault", None),
        EC_SP_ALIGN => ("SP alignment fault", None),
        EC_ILLEGAL_STATE => ("illegal execution state", None),
        EC_BRK => ("breakpoint", None),
        _ => ("synchronous exception", None),
    };
    crate::userspace::kill_faulting_task(what, frame.elr, addr)
}

fn kernel_fault(what: &str, esr: u64, frame: &TaskContext) -> ! {
    use core::fmt::Write;
    let mut line: heapless::String<160> = heapless::String::new();
    let _ = write!(
        line,
        "\n[EXC] Kernel {}: ESR=0x{:x} (EC 0x{:x}) ELR=0x{:x} FAR=0x{:x}\n",
        what,
        esr,
        esr >> 26,
        frame.elr,
        far()
    );
    crate::serial::write_str(&line);
    panic!("unhandled kernel exception");
}
