//! amd64 task context: the register frame the timer / yield / syscall
//! stubs push, and the initial frames for new tasks.

/// Software-interrupt vector a task uses to give up the CPU voluntarily
/// (blocking IPC, sleep, exit). Same save/switch path as the timer, minus
/// the EOI — there is no interrupt controller state to acknowledge.
pub const YIELD_VECTOR: u8 = 0x31;

/// Saved register context for a task (in order pushed by the stubs)
#[repr(C)]
pub struct TaskContext {
    pub r15: u64,
    pub r14: u64,
    pub r13: u64,
    pub r12: u64,
    pub r11: u64,
    pub r10: u64,
    pub r9: u64,
    pub r8: u64,
    pub rdi: u64,
    pub rsi: u64,
    pub rbp: u64,
    pub rbx: u64,
    pub rdx: u64,
    pub rcx: u64,
    pub rax: u64,
    // Below this are the interrupt frame (pushed by CPU on interrupt)
    pub rip: u64,
    pub cs: u64,
    pub rflags: u64,
    pub rsp: u64,
    pub ss: u64,
}

impl TaskContext {
    const fn zeroed() -> Self {
        Self {
            r15: 0, r14: 0, r13: 0, r12: 0, r11: 0, r10: 0, r9: 0, r8: 0,
            rdi: 0, rsi: 0, rbp: 0, rbx: 0, rdx: 0, rcx: 0, rax: 0,
            rip: 0, cs: 0, rflags: 0, rsp: 0, ss: 0,
        }
    }
}

/// Initial frame for a kernel task: IRETQ lands in `entry` at CPL 0 with
/// interrupts enabled
pub fn kernel_context(entry: u64, stack_top: u64) -> TaskContext {
    TaskContext {
        rip: entry,
        cs: 0x08,       // GDT kernel code segment
        rflags: 0x202,  // IF (interrupts enabled) + reserved bit 1
        rsp: stack_top, // IRETQ loads RSP from here
        ss: 0x10,       // GDT kernel data segment
        ..TaskContext::zeroed()
    }
}

/// Initial frame for a ring-3 task: IRETQ lands in `entry` at CPL 3 on
/// `user_sp`, with `arg` in RDI
pub fn user_context(entry: u64, user_sp: u64, arg: u64, _kstack_top: u64) -> TaskContext {
    TaskContext {
        rip: entry,
        cs: super::gdt::USER_CS,
        rflags: 0x202, // IF set, IOPL 0: no port I/O from ring 3
        rsp: user_sp,
        ss: super::gdt::USER_SS,
        rdi: arg,
        ..TaskContext::zeroed()
    }
}

/// Give up the CPU (see `scheduler::yield_now`)
pub fn yield_now() {
    unsafe {
        core::arch::asm!("int {v}", v = const YIELD_VECTOR);
    }
}

/// Before running a ring-3 task: traps from ring 3 must land on its own
/// kernel stack
pub fn on_switch_to_user(cpu: usize, kstack_top: u64) {
    super::gdt::set_kernel_stack(cpu, kstack_top);
}

/// Timer / reschedule-IPI entry from the `timer_handler` stub: acknowledge
/// the interrupt (LAPIC EOI in APIC mode, PIC otherwise), then switch.
///
/// # Safety
/// Only from the stub, with RSP pointing at the saved TaskContext.
#[no_mangle]
pub unsafe extern "C" fn schedule_and_switch(current_rsp: u64) -> u64 {
    super::interrupts::irq_eoi(0);
    crate::scheduler::switch_from_interrupt(current_rsp, true)
}
