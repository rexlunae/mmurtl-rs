//! Interrupt Descriptor Table — Handles CPU exceptions and hardware IRQs.
//!
//! Sets up IDT entries for all CPU exceptions and hardware IRQs.
//! Explicit assembly entries preserve CPU frames and clear live registers
//! before ordinary Rust handlers run.

use x86_64::structures::idt::{InterruptDescriptorTable, InterruptStackFrame, PageFaultErrorCode};
use x86_64::instructions::port::Port;
use spin::Lazy;

/// PIC base interrupt numbers (offset remapping)
pub const PIC_1_OFFSET: u8 = 32;
pub const PIC_2_OFFSET: u8 = PIC_1_OFFSET + 8;

/// PIC command/data ports
const PIC1_COMMAND: u16 = 0x20;
const PIC1_DATA: u16 = 0x21;
const PIC2_COMMAND: u16 = 0xA0;
const PIC2_DATA: u16 = 0xA1;

/// Initialize the PIC (8259) with remapped interrupt vectors.
pub fn init_pic() {
    unsafe {
        let mut cmd1: Port<u8> = Port::new(PIC1_COMMAND);
        let mut data1: Port<u8> = Port::new(PIC1_DATA);
        let mut cmd2: Port<u8> = Port::new(PIC2_COMMAND);
        let mut data2: Port<u8> = Port::new(PIC2_DATA);

        // ICW1: begin initialization
        cmd1.write(0x11u8);
        cmd2.write(0x11u8);

        // ICW2: remap IRQ base vectors
        data1.write(PIC_1_OFFSET); // master: IRQ0 -> INT 32
        data2.write(PIC_2_OFFSET); // slave:  IRQ8 -> INT 40

        // ICW3: cascade configuration
        data1.write(0x04u8); // master: slave at IRQ2
        data2.write(0x02u8); // slave:  cascade identity

        // ICW4: 8086 mode
        data1.write(0x01u8);
        data2.write(0x01u8);

        // Mask all interrupts
        data1.write(0xFBu8); // mask all except IRQ2 (cascade)
        data2.write(0xFFu8); // mask all slave interrupts

        crate::serial::write_line("[PIC] Remapped to 0x20/0x28, all IRQs masked");
    }
}

/// Send End-Of-Interrupt to the PIC
pub unsafe fn eoi(irq: u8) {
    if irq >= 8 {
        let mut slave_port: Port<u8> = Port::new(PIC2_COMMAND);
        slave_port.write(0x20u8); // EOI to slave
    }
    let mut master_port: Port<u8> = Port::new(PIC1_COMMAND);
    master_port.write(0x20u8); // EOI to master
}

/// Fully mask the legacy PIC — used once the APIC takes over
pub fn mask_pic() {
    unsafe {
        let mut data1: Port<u8> = Port::new(PIC1_DATA);
        let mut data2: Port<u8> = Port::new(PIC2_DATA);
        data1.write(0xFFu8);
        data2.write(0xFFu8);
    }
    crate::serial::write_line("[PIC] Fully masked (APIC mode)");
}

/// Send end-of-interrupt for a hardware IRQ, APIC-aware.
///
/// In APIC mode all IRQs (LAPIC timer, IOAPIC-routed keyboard) are
/// acknowledged at the Local APIC; otherwise fall back to the 8259 PIC.
pub fn irq_eoi(irq: u8) {
    if crate::arch::apic::enabled() {
        crate::arch::apic::eoi();
    } else {
        unsafe { eoi(irq) };
    }
}

/// Legacy console IRQs when no Local/I/O APIC is available.
pub fn unmask_console() {
    unsafe {
        let mut mask = Port::<u8>::new(PIC1_DATA);
        let bits = mask.read();
        mask.write(bits & !0x12); // IRQ1 keyboard and IRQ4 COM1
    }
}

fn com1_handler(_frame: &mut InterruptStackFrame) {
    super::protection::clear_access();
    crate::arch::serial::handle_rx_irq();
    irq_eoi(4);
}

/// Test-only DPL0 interrupt: deliberately change carry in the handler.
/// IRET must restore the interrupted comparison's CF before SBB resumes.
fn nospec_flags_handler(_frame: &mut InterruptStackFrame) {
    super::protection::clear_access();
    unsafe { core::arch::asm!("cmc", options(nostack)); }
}

pub fn nospec_interrupt_self_test() {
    for (index, size, expected) in [(0usize, 1usize, usize::MAX), (1, 1, 0), (usize::MAX, 11, 0)] {
        let mask: usize;
        unsafe { core::arch::asm!("cmp {index}, {size}", "int 0x84", "sbb {mask}, {mask}",
            index = in(reg) index, size = in(reg) size, mask = lateout(reg) mask); }
        assert_eq!(mask, expected, "interrupt changed nospec carry mask");
    }
    crate::serial::write_line("[TEST] nospec CMP/interrupt/SBB preserves bounds mask");
}

/// Hardware interrupt numbers
#[derive(Debug, Clone, Copy)]
#[repr(u8)]
pub enum InterruptIndex {
    Timer        = PIC_1_OFFSET + 0,
    Keyboard     = PIC_1_OFFSET + 1,
    Cascade      = PIC_1_OFFSET + 2,
    Com2         = PIC_1_OFFSET + 3,
    Com1         = PIC_1_OFFSET + 4,
    Lpt2         = PIC_1_OFFSET + 5,
    Floppy       = PIC_1_OFFSET + 6,
    Lpt1         = PIC_1_OFFSET + 7,
    Rtc          = PIC_2_OFFSET + 0,
    Free1        = PIC_2_OFFSET + 1,
    Free2        = PIC_2_OFFSET + 2,
    Free3        = PIC_2_OFFSET + 3,
    Mouse        = PIC_2_OFFSET + 4,
    CoPro        = PIC_2_OFFSET + 5,
    PrimaryAta   = PIC_2_OFFSET + 6,
    SecondaryAta = PIC_2_OFFSET + 7,
}

impl InterruptIndex {
    pub fn as_u8(self) -> u8 { self as u8 }
    pub fn as_usize(self) -> usize { self as u8 as usize }
}

// ========================================================================
// Interrupt Handler Definitions
// Assembly saves/clears all GPRs; ordinary Rust handlers borrow the CPU frame.
// ========================================================================

/// Whether an exception was raised by ring-3 code
fn from_user(frame: &InterruptStackFrame) -> bool {
    frame.code_segment & 3 == 3
}

/// Kill the current user task for a fault (see
/// `userspace::kill_faulting_task`)
fn kill_user_task(frame: &InterruptStackFrame, what: &str, addr: Option<u64>) -> ! {
    crate::userspace::kill_faulting_task(what, frame.instruction_pointer.as_u64(), addr)
}

fn divide_error_handler(stack_frame: &mut InterruptStackFrame) {
    super::protection::clear_access();
    if from_user(&stack_frame) {
        kill_user_task(&stack_frame, "#DE divide error", None);
    }
    panic!("DIVIDE ERROR\n{:#?}", stack_frame);
}

static DEBUG_EVENTS: core::sync::atomic::AtomicU64 = core::sync::atomic::AtomicU64::new(0);
static NMI_EVENTS: core::sync::atomic::AtomicU64 = core::sync::atomic::AtomicU64::new(0);
static CPU_NMIS: [core::sync::atomic::AtomicU64; crate::scheduler::MAX_CPUS] =
    [const { core::sync::atomic::AtomicU64::new(0) }; crate::scheduler::MAX_CPUS];
pub fn current_nmi_count() -> u64 { CPU_NMIS[super::cpu_index()].load(core::sync::atomic::Ordering::Relaxed) }

fn debug_handler(stack_frame: &mut InterruptStackFrame) {
    super::protection::clear_access();
    // The saved CS describes where #DB was delivered, not who armed TF.
    // A deferred user trap delivered ANYWHERE inside a kernel entry has
    // CPL0 here. Keep that entire path independent of scheduler locks,
    // including traps past the first instruction and during stack switches.
    if !from_user(&stack_frame) {
        let rip = stack_frame.instruction_pointer.as_u64();
        if [super::syscall::entry_address(), timer_handler as *const () as u64,
            yield_handler as *const () as u64].contains(&rip) { return; }
        unsafe {
            core::arch::asm!("mov dr6, {}", in(reg) 0xffff0ff0u64, options(nostack));
            core::arch::asm!("mov dr7, {}", in(reg) 0u64, options(nostack));
            stack_frame.as_mut().update(|frame| frame.cpu_flags &= !(1 << 8));
        }
        DEBUG_EVENTS.fetch_add(1, core::sync::atomic::Ordering::Relaxed);
        return;
    }
    // Only a trap actually delivered at CPL3 may enter the scheduler.
    unsafe { core::arch::asm!("mov dr6, {}", in(reg) 0xffff0ff0u64, options(nostack)); }
    kill_user_task(&stack_frame, "#DB debug trap", None);
}

pub fn debug_self_test() {
    // Deterministic alignment-check policy on the BSP; APs set it when
    // loading their exception tables too.
    enable_alignment_checks();
    let before = DEBUG_EVENTS.load(core::sync::atomic::Ordering::Relaxed);
    unsafe { core::arch::asm!("pushfq", "or qword ptr [rsp], 0x100", "popfq", "nop"); }
    assert!(DEBUG_EVENTS.load(core::sync::atomic::Ordering::Relaxed) > before);
}

fn nmi_handler(_stack_frame: &mut InterruptStackFrame) {
    super::protection::clear_access();
    // NMI can interrupt a serial writer or allocator while it holds a
    // lock. Count it without printing, allocating, or entering the scheduler.
    NMI_EVENTS.fetch_add(1, core::sync::atomic::Ordering::Relaxed);
    CPU_NMIS[super::cpu_index()].fetch_add(1, core::sync::atomic::Ordering::Relaxed);
}

fn breakpoint_handler(stack_frame: &mut InterruptStackFrame) {
    super::protection::clear_access();
    crate::serial::write_str("[BREAKPOINT] at ");
    crate::kptr::write(stack_frame.instruction_pointer.as_u64());
    crate::serial::write_str("\n");
}

fn overflow_handler(stack_frame: &mut InterruptStackFrame) {
    super::protection::clear_access();
    panic!("OVERFLOW\n{:#?}", stack_frame);
}

fn bound_range_handler(stack_frame: &mut InterruptStackFrame) {
    super::protection::clear_access();
    panic!("BOUND RANGE EXCEEDED\n{:#?}", stack_frame);
}

fn invalid_opcode_handler(stack_frame: &mut InterruptStackFrame) {
    super::protection::clear_access();
    if from_user(&stack_frame) {
        kill_user_task(&stack_frame, "#UD invalid opcode", None);
    }
    panic!("INVALID OPCODE\n{:#?}", stack_frame);
}

fn device_not_available_handler(stack_frame: &mut InterruptStackFrame) {
    super::protection::clear_access();
    if from_user(&stack_frame) { kill_user_task(&stack_frame, "#NM FP/SIMD disabled", None); }
    panic!("Kernel used disabled FP/SIMD\n{:#?}", stack_frame);
}

fn double_fault_handler(
    _stack_frame: &mut InterruptStackFrame,
    _error_code: u64,
) -> ! {
    super::protection::clear_access();
    crate::memory::stacks::kernel_stack_fatal();
}

fn invalid_tss_handler(
    stack_frame: &mut InterruptStackFrame,
    error_code: u64,
) {
    super::protection::clear_access();
    panic!("INVALID TSS error_code={:#x}\n{:#?}", error_code, stack_frame);
}

fn segment_not_present_handler(
    stack_frame: &mut InterruptStackFrame,
    error_code: u64,
) {
    super::protection::clear_access();
    panic!("SEGMENT NOT PRESENT error_code={:#x}\n{:#?}", error_code, stack_frame);
}

fn stack_segment_handler(
    stack_frame: &mut InterruptStackFrame,
    error_code: u64,
) {
    super::protection::clear_access();
    if from_user(&stack_frame) {
        kill_user_task(&stack_frame, "#SS stack fault", None);
    }
    panic!("STACK SEGMENT FAULT error_code={:#x}\n{:#?}", error_code, stack_frame);
}

fn general_protection_handler(
    stack_frame: &mut InterruptStackFrame,
    error_code: u64,
) {
    super::protection::clear_access();
    if from_user(&stack_frame) {
        kill_user_task(&stack_frame, "#GP general protection fault", None);
    }
    if let Some(pc) = super::microcode_msr::fixup(stack_frame.instruction_pointer.as_u64()) {
        unsafe { stack_frame.as_mut().update(|f| f.instruction_pointer = x86_64::VirtAddr::new(pc)); }
        return;
    }
    if error_code == 0 {
        let pc = stack_frame.instruction_pointer.as_u64();
        if let Some(fixup) = super::uaccess::fixup(pc).or_else(|| super::protection::test_gp_fixup(pc, error_code)) {
            super::user_access_end();
            unsafe { stack_frame.as_mut().update(|frame| {
                frame.instruction_pointer = x86_64::VirtAddr::new(fixup);
                frame.cpu_flags &= !(1 << 18);
            }); }
            return;
        }
    }
    panic!("GENERAL PROTECTION FAULT error_code={:#x}\n{:#?}", error_code, stack_frame);
}

fn page_fault_handler(
    stack_frame: &mut InterruptStackFrame,
    error_code: PageFaultErrorCode,
) {
    super::protection::clear_access();
    use x86_64::registers::control::Cr2;
    let address = Cr2::read();
    if !from_user(&stack_frame) {
        let pc = stack_frame.instruction_pointer.as_u64();
        if let Some(fixup) = super::uaccess::fixup(pc).or_else(||
            super::protection::test_fixup(pc, address.as_u64(), error_code.bits())) {
            super::user_access_end();
            unsafe { stack_frame.as_mut().update(|frame| {
                frame.instruction_pointer = x86_64::VirtAddr::new(fixup);
                frame.cpu_flags &= !(1 << 18); // don't restore a faulted SMAP window
            }); }
            return;
        }
    }

    if from_user(&stack_frame) {
        let what = if error_code.contains(PageFaultErrorCode::PROTECTION_VIOLATION) {
            "#PF protection violation"
        } else {
            "#PF not-present page"
        };
        kill_user_task(&stack_frame, what, Some(address.as_u64()));
    }

    crate::serial::write_str("\n[PAGE FAULT]\n");
    crate::serial::write_str("  Address: ");
    crate::kptr::write(address.as_u64());
    crate::serial::write_str("\n");
    crate::serial::write_str("  Error code:");
    crate::serial::write_hex_byte(error_code.bits() as u8);
    crate::serial::write_str("\n");

    panic!("Unresolved page fault");
}

fn x87_fpu_handler(stack_frame: &mut InterruptStackFrame) {
    super::protection::clear_access();
    if from_user(&stack_frame) { kill_user_task(&stack_frame, "#MF floating-point exception", None); }
    panic!("x87 FPU ERROR\n{:#?}", stack_frame);
}

fn alignment_check_handler(
    stack_frame: &mut InterruptStackFrame,
    error_code: u64,
) {
    super::protection::clear_access();
    if from_user(&stack_frame) { kill_user_task(&stack_frame, "#AC alignment check", None); }
    panic!("ALIGNMENT CHECK error_code={:#x}\n{:#?}", error_code, stack_frame);
}

// A machine check can interrupt any lock holder. Emergency output must
// not use the normal serial mutex, heap, scheduler, or formatting code.
fn emergency_text(text: &str) {
    for byte in text.bytes() {
        unsafe {
            let mut ready = false;
            for _ in 0..10_000 {
                if Port::<u8>::new(0x3fd).read() & 0x20 != 0 { ready = true; break; }
            }
            if !ready { return; }
            Port::<u8>::new(0x3f8).write(byte);
        }
    }
}
fn emergency_hex(value: u64) {
    let mut text = [0u8; 18]; text[..2].copy_from_slice(b"0x");
    for i in 0..16 { text[2 + i] = b"0123456789abcdef"[((value >> (60 - i * 4)) & 15) as usize]; }
    emergency_text(core::str::from_utf8(&text).unwrap());
}
unsafe fn emergency_msr(index: u32) -> u64 {
    let (lo, hi): (u32, u32);
    core::arch::asm!("rdmsr", in("ecx") index, out("eax") lo, out("edx") hi, options(nostack));
    (hi as u64) << 32 | lo as u64
}
/// Regression image only: seed one synthetic MCA bank, then enter the
/// fatal handler through its real guarded IST gate. No production entry.
#[cfg(feature = "test-machine-check")]
pub fn test_machine_check() -> ! {
    assert!(unsafe { core::arch::x86_64::__cpuid(1) }.edx & (1 << 14) != 0);
    let cap = unsafe { emergency_msr(0x179) };
    assert!(cap & 0xff != 0);
    let pointer = test_machine_check as *const () as u64;
    assert!(super::microcode_msr::write(0x402, pointer));
    assert!(super::microcode_msr::write(0x403, pointer));
    assert!(super::microcode_msr::write(0x401, (1 << 63) | (1 << 58) | (1 << 59)));
    assert_eq!(unsafe { emergency_msr(0x402) }, pointer);
    assert_eq!(unsafe { emergency_msr(0x403) }, pointer);
    if crate::kptr::RAW_ADDRESSES {
        emergency_text("[MCE TEST] injected="); emergency_hex(pointer); emergency_text("\n");
    }
    unsafe { core::arch::asm!("int 0x12", options(noreturn)); }
}

fn machine_check_handler(stack_frame: &mut InterruptStackFrame) -> ! {
    super::protection::clear_access();
    emergency_text("\n[MCE] fatal machine check at ");
    // Machine checks cannot take formatting/RNG/serial locks. Redact with
    // a constant string rather than invoking the normal pointer formatter.
    if crate::kptr::RAW_ADDRESSES { emergency_hex(stack_frame.instruction_pointer.as_u64()); }
    else { emergency_text("(ptr)"); }
    // These MSRs exist only when CPUID advertises MCA.
    if unsafe { core::arch::x86_64::__cpuid(1) }.edx & (1 << 14) != 0 {
        let cap = unsafe { emergency_msr(0x179) };
        emergency_text(" MCG_STATUS="); emergency_hex(unsafe { emergency_msr(0x17a) });
        for bank in 0..(cap as u32 & 0xff) {
            let status = unsafe { emergency_msr(0x401 + bank * 4) };
            if status & (1 << 63) == 0 { continue; }
            emergency_text("\n[MCE] bank="); emergency_hex(bank as u64);
            emergency_text(" status="); emergency_hex(status);
            if status & (1 << 58) != 0 {
                emergency_text(" addr=");
                if crate::kptr::RAW_ADDRESSES { emergency_hex(unsafe { emergency_msr(0x402 + bank * 4) }); }
                else { emergency_text("(ptr)"); }
            }
            if status & (1 << 59) != 0 {
                emergency_text(" misc=");
                if crate::kptr::RAW_ADDRESSES { emergency_hex(unsafe { emergency_msr(0x403 + bank * 4) }); }
                else { emergency_text("(restricted)"); }
            }
        }
    }
    emergency_text("\n[MCE] CPU halted\n");
    loop { unsafe { core::arch::asm!("cli", "hlt", options(nomem, nostack)); } }
}

fn simd_fpu_handler(stack_frame: &mut InterruptStackFrame) {
    super::protection::clear_access();
    if from_user(&stack_frame) { kill_user_task(&stack_frame, "#XM SIMD exception", None); }
    panic!("SIMD FPU ERROR\n{:#?}", stack_frame);
}

fn virtualization_handler(stack_frame: &mut InterruptStackFrame) {
    super::protection::clear_access();
    panic!("VIRTUALIZATION EXCEPTION\n{:#?}", stack_frame);
}

fn security_exception_handler(
    stack_frame: &mut InterruptStackFrame,
    error_code: u64,
) {
    super::protection::clear_access();
    panic!("SECURITY EXCEPTION error_code={:#x}\n{:#?}", error_code, stack_frame);
}

// ========================================================================
// Hardware Interrupt Handlers
// ========================================================================

/// Timer interrupt handler — defined in global assembly to avoid naked function restrictions.
///
/// Saves all registers, calls the Rust scheduler, then restores and IRETQs to the
/// next scheduled task. This is the heart of preemptive multitasking.
core::arch::global_asm!(
    ".globl timer_handler",
    "timer_handler:",
    "cmp byte ptr [rip + SMAP_ENTRY_ENABLED], 0",
    "je 2f",
    "clac",
    "2:",
    // Push all general-purpose registers (TaskContext layout)
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
    // RSP now points to saved TaskContext — pass it to the scheduler
    "mov rdi, rsp",
    "xor eax, eax",
    "xor ecx, ecx",
    "xor edx, edx",
    "xor ebx, ebx",
    "xor ebp, ebp",
    "xor esi, esi",
    "xor r8d, r8d",
    "xor r9d, r9d",
    "xor r10d, r10d",
    "xor r11d, r11d",
    "xor r12d, r12d",
    "xor r13d, r13d",
    "xor r14d, r14d",
    "xor r15d, r15d",
    "cld",
    // Call the Rust scheduler — returns new RSP in RAX.
    // (EOI is sent inside schedule_and_switch, APIC- or PIC-appropriate.)
    // NOTE: schedule_and_switch returns with the scheduler lock still
    // held, so no other CPU can resume the outgoing task while we are
    // still executing on its stack.
    "mov rbx, rsp",
    "mov rsi, [rsp + 128]",
    "and esi, 3",
    "call kernel_stack_offset",
    "sub rsp, rax",
    "mov rdi, rbx",
    "call schedule_and_switch",
    // Shared tail with yield_handler: switch stacks, unlock, restore
    "context_switch_tail:",
    // Switch to the new task's stack
    "mov rsp, rax",
    // Now that we're off the old task's stack, release the scheduler lock
    // (clobbers caller-saved registers — fine, we restore from the stack)
    "call scheduler_unlock",
    "mov rdi, rsp",
    "mov rsi, [rsp + 128]",
    "and esi, 3",
    "call kernel_stack_erase_bottom",
    "test rax, rax",
    "jz 3f",
    "mov rdi, rax",
    "mov rcx, rsp",
    "sub rcx, rdi",
    "xor eax, eax",
    "rep stosb",
    "3:",
    // Restore registers
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
    "pop rax",
    // Return to the next task
    "iretq",
    "int3",

    // Voluntary yield (int YIELD_VECTOR): identical context save, but
    // calls yield_and_switch (no EOI — nothing to acknowledge) and then
    // shares the switch/unlock/restore tail above.
    ".globl yield_handler",
    "yield_handler:",
    "cmp byte ptr [rip + SMAP_ENTRY_ENABLED], 0",
    "je 2f",
    "clac",
    "2:",
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
    "xor eax, eax",
    "xor ecx, ecx",
    "xor edx, edx",
    "xor ebx, ebx",
    "xor ebp, ebp",
    "xor esi, esi",
    "xor r8d, r8d",
    "xor r9d, r9d",
    "xor r10d, r10d",
    "xor r11d, r11d",
    "xor r12d, r12d",
    "xor r13d, r13d",
    "xor r14d, r14d",
    "xor r15d, r15d",
    "cld",
    "call yield_and_switch",
    "jmp context_switch_tail"
);

fn keyboard_handler(_stack_frame: &mut InterruptStackFrame) {
    super::protection::clear_access();
    let scancode: u8 = unsafe {
        let mut status_port: Port<u8> = Port::new(0x64);
        while (status_port.read() & 1) == 0 {}

        let mut data_port: Port<u8> = Port::new(0x60);
        data_port.read()
    };
    // Feed the keyboard driver (translates + queues; no printing here)
    crate::keyboard::handle_scancode(scancode);
    irq_eoi(InterruptIndex::Keyboard.as_u8() - PIC_1_OFFSET);
}

/// LAPIC spurious interrupt — no EOI must be sent
fn spurious_handler(_stack_frame: &mut InterruptStackFrame) {
    super::protection::clear_access();
}

/// LAPIC error interrupt
fn apic_error_handler(_stack_frame: &mut InterruptStackFrame) {
    super::protection::clear_access();
    crate::serial::write_line("[APIC] Error interrupt received");
    crate::arch::apic::eoi();
}

// ========================================================================
// IDT Initialization
// ========================================================================

// A uniform error/vector prefix makes every gate's saved GPR frame identical.
// Hardware has aligned RSP to 16 bytes; the prefix plus 15 GPRs preserves
// SysV call alignment. The CPU frame is borrowed in place, including fixups.
core::arch::global_asm!(r#"
.macro INTERRUPT_STUB name, vector, error
.global \name
\name:
    cmp byte ptr [rip + SMAP_ENTRY_ENABLED], 0
    je 1f
    clac
1:
.if !\error
    push 0
.endif
    push \vector
    jmp x86_exception_common
.endm
INTERRUPT_STUB entry_divide_error_handler, 0, 0
INTERRUPT_STUB entry_debug_handler, 1, 0
INTERRUPT_STUB entry_nmi_handler, 2, 0
INTERRUPT_STUB entry_breakpoint_handler, 3, 0
INTERRUPT_STUB entry_overflow_handler, 4, 0
INTERRUPT_STUB entry_bound_range_handler, 5, 0
INTERRUPT_STUB entry_invalid_opcode_handler, 6, 0
INTERRUPT_STUB entry_device_not_available_handler, 7, 0
INTERRUPT_STUB entry_double_fault_handler, 8, 1
INTERRUPT_STUB entry_invalid_tss_handler, 10, 1
INTERRUPT_STUB entry_segment_not_present_handler, 11, 1
INTERRUPT_STUB entry_stack_segment_handler, 12, 1
INTERRUPT_STUB entry_general_protection_handler, 13, 1
INTERRUPT_STUB entry_page_fault_handler, 14, 1
INTERRUPT_STUB entry_x87_fpu_handler, 16, 0
INTERRUPT_STUB entry_alignment_check_handler, 17, 1
INTERRUPT_STUB entry_machine_check_handler, 18, 0
INTERRUPT_STUB entry_simd_fpu_handler, 19, 0
INTERRUPT_STUB entry_virtualization_handler, 20, 0
INTERRUPT_STUB entry_security_exception_handler, 30, 1
INTERRUPT_STUB entry_keyboard_handler, 33, 0
INTERRUPT_STUB entry_com1_handler, 36, 0
INTERRUPT_STUB entry_apic_error_handler, 254, 0
INTERRUPT_STUB entry_spurious_handler, 255, 0
INTERRUPT_STUB entry_nospec_flags_handler, 132, 0
INTERRUPT_STUB entry_tlb_handler, 50, 0

.global x86_exception_common
x86_exception_common:
    push rax
    push rcx
    push rdx
    push rbx
    push rbp
    push rsi
    push rdi
    push r8
    push r9
    push r10
    push r11
    push r12
    push r13
    push r14
    push r15
    mov rdi, [rsp + 120]
    lea rsi, [rsp + 136]
    mov rdx, [rsp + 128]
    xor eax, eax
    xor ecx, ecx
    xor ebx, ebx
    xor ebp, ebp
    xor r8d, r8d
    xor r9d, r9d
    xor r10d, r10d
    xor r11d, r11d
    xor r12d, r12d
    xor r13d, r13d
    xor r14d, r14d
    xor r15d, r15d
    cld
    mov rbx, rsp
    mov r12, rdi
    mov r13, rsi
    mov r14, rdx
    mov rdi, rsp
    mov rsi, [rsp + 144]
    and esi, 3
    call kernel_stack_offset
    sub rsp, rax
    mov rdi, r12
    mov rsi, r13
    mov rdx, r14
    call x86_exception_dispatch
    mov rsp, rbx
    mov rdi, rsp
    mov rsi, [rsp + 144]
    and esi, 3
    call kernel_stack_erase_bottom
    test rax, rax
    jz 3f
    mov rdi, rax
    mov rcx, rsp
    sub rcx, rdi
    xor eax, eax
    rep stosb
3:
    pop r15
    pop r14
    pop r13
    pop r12
    pop r11
    pop r10
    pop r9
    pop r8
    pop rdi
    pop rsi
    pop rbp
    pop rbx
    pop rdx
    pop rcx
    pop rax
    add rsp, 16
    iretq
    int3
"#);
extern "C" {
    fn entry_divide_error_handler();
    fn entry_debug_handler();
    fn entry_nmi_handler();
    fn entry_breakpoint_handler();
    fn entry_overflow_handler();
    fn entry_bound_range_handler();
    fn entry_invalid_opcode_handler();
    fn entry_device_not_available_handler();
    fn entry_double_fault_handler();
    fn entry_invalid_tss_handler();
    fn entry_segment_not_present_handler();
    fn entry_stack_segment_handler();
    fn entry_general_protection_handler();
    fn entry_page_fault_handler();
    fn entry_x87_fpu_handler();
    fn entry_alignment_check_handler();
    fn entry_machine_check_handler();
    fn entry_simd_fpu_handler();
    fn entry_virtualization_handler();
    fn entry_security_exception_handler();
    fn entry_keyboard_handler();
    fn entry_com1_handler();
    fn entry_apic_error_handler();
    fn entry_spurious_handler();
    fn entry_nospec_flags_handler();
    fn entry_tlb_handler();
}
#[no_mangle]
unsafe extern "C" fn x86_exception_dispatch(vector: u64, frame: &mut InterruptStackFrame, error: u64) {
    match vector {
        0 => divide_error_handler(frame),
        1 => debug_handler(frame),
        2 => nmi_handler(frame),
        3 => breakpoint_handler(frame),
        4 => overflow_handler(frame),
        5 => bound_range_handler(frame),
        6 => invalid_opcode_handler(frame),
        7 => device_not_available_handler(frame),
        8 => double_fault_handler(frame, error),
        10 => invalid_tss_handler(frame, error),
        11 => segment_not_present_handler(frame, error),
        12 => stack_segment_handler(frame, error),
        13 => general_protection_handler(frame, error),
        14 => page_fault_handler(frame, PageFaultErrorCode::from_bits_truncate(error)),
        16 => x87_fpu_handler(frame),
        17 => alignment_check_handler(frame, error),
        18 => machine_check_handler(frame),
        19 => simd_fpu_handler(frame),
        20 => virtualization_handler(frame),
        30 => security_exception_handler(frame, error),
        33 => keyboard_handler(frame),
        36 => com1_handler(frame),
        254 => apic_error_handler(frame),
        255 => spurious_handler(frame),
        132 => nospec_flags_handler(frame),
        50 => super::wx::tlb_handler(),
        _ => panic!("unregistered exception stub"),
    }
}


/// Timer handler — defined in global_asm above
extern "C" {
    fn timer_handler();
    fn yield_handler();
}

/// The IDT — initialized once at boot
static mut IDT_TABLE: InterruptDescriptorTable = InterruptDescriptorTable::new();
static IDT: Lazy<()> = Lazy::new(|| unsafe {
    // Initialize in place; returning a 4 KiB IDT by value used multiple
    // stack copies in the Once/Lazy chain.
    let idt = &mut *core::ptr::addr_of_mut!(IDT_TABLE);

    // CPU exceptions
    idt.divide_error.set_handler_addr(x86_64::VirtAddr::new(entry_divide_error_handler as *const () as u64));
    unsafe {
        idt.debug.set_handler_addr(x86_64::VirtAddr::new(entry_debug_handler as *const () as u64)).set_stack_index(super::gdt::DEBUG_IST_INDEX);
        idt.non_maskable_interrupt.set_handler_addr(x86_64::VirtAddr::new(entry_nmi_handler as *const () as u64)).set_stack_index(super::gdt::NMI_IST_INDEX);
        idt.machine_check.set_handler_addr(x86_64::VirtAddr::new(entry_machine_check_handler as *const () as u64)).set_stack_index(super::gdt::MACHINE_CHECK_IST_INDEX);
    }
    idt.breakpoint.set_handler_addr(x86_64::VirtAddr::new(entry_breakpoint_handler as *const () as u64));
    idt.overflow.set_handler_addr(x86_64::VirtAddr::new(entry_overflow_handler as *const () as u64));
    idt.bound_range_exceeded.set_handler_addr(x86_64::VirtAddr::new(entry_bound_range_handler as *const () as u64));
    idt.invalid_opcode.set_handler_addr(x86_64::VirtAddr::new(entry_invalid_opcode_handler as *const () as u64));
    idt.device_not_available.set_handler_addr(x86_64::VirtAddr::new(entry_device_not_available_handler as *const () as u64));
    idt.invalid_tss.set_handler_addr(x86_64::VirtAddr::new(entry_invalid_tss_handler as *const () as u64));
    idt.segment_not_present.set_handler_addr(x86_64::VirtAddr::new(entry_segment_not_present_handler as *const () as u64));
    idt.stack_segment_fault.set_handler_addr(x86_64::VirtAddr::new(entry_stack_segment_handler as *const () as u64));
    idt.general_protection_fault.set_handler_addr(x86_64::VirtAddr::new(entry_general_protection_handler as *const () as u64));
    idt.page_fault.set_handler_addr(x86_64::VirtAddr::new(entry_page_fault_handler as *const () as u64));
    idt[0x32].set_handler_addr(x86_64::VirtAddr::new(entry_tlb_handler as *const () as u64));
    idt[0x84].set_handler_addr(x86_64::VirtAddr::new(entry_nospec_flags_handler as *const () as u64));
    unsafe { idt[0x83].set_handler_addr(x86_64::VirtAddr::new(super::protection::test_gate_address())); }
    idt.x87_floating_point.set_handler_addr(x86_64::VirtAddr::new(entry_x87_fpu_handler as *const () as u64));
    idt.alignment_check.set_handler_addr(x86_64::VirtAddr::new(entry_alignment_check_handler as *const () as u64));
    idt.simd_floating_point.set_handler_addr(x86_64::VirtAddr::new(entry_simd_fpu_handler as *const () as u64));
    idt.virtualization.set_handler_addr(x86_64::VirtAddr::new(entry_virtualization_handler as *const () as u64));
    idt.security_exception.set_handler_addr(x86_64::VirtAddr::new(entry_security_exception_handler as *const () as u64));

    // Hardware interrupts
    unsafe {
        idt[InterruptIndex::Timer.as_usize()].set_handler_addr(
            x86_64::VirtAddr::new(timer_handler as usize as u64)
        );
        // Reschedule IPI — identical save/schedule/restore flow
        idt[crate::arch::apic::RESCHED_VECTOR as usize].set_handler_addr(
            x86_64::VirtAddr::new(timer_handler as usize as u64)
        );
        // Voluntary yield (blocking IPC, sleep, exit) — kernel only (DPL 0)
        idt[super::context::YIELD_VECTOR as usize].set_handler_addr(
            x86_64::VirtAddr::new(yield_handler as usize as u64)
        );
        // Syscalls — the only gate ring 3 may invoke (DPL 3)
        idt[super::syscall::SYSCALL_VECTOR as usize]
            .set_handler_addr(x86_64::VirtAddr::new(super::syscall::entry_address()))
            .set_privilege_level(x86_64::PrivilegeLevel::Ring3);
    }
    idt[InterruptIndex::Keyboard.as_usize()].set_handler_addr(x86_64::VirtAddr::new(entry_keyboard_handler as *const () as u64));
    idt[InterruptIndex::Com1.as_usize()].set_handler_addr(x86_64::VirtAddr::new(entry_com1_handler as *const () as u64));

    // LAPIC vectors
    idt[crate::arch::apic::ERROR_VECTOR as usize].set_handler_addr(x86_64::VirtAddr::new(entry_apic_error_handler as *const () as u64));
    idt[crate::arch::apic::SPURIOUS_VECTOR as usize].set_handler_addr(x86_64::VirtAddr::new(entry_spurious_handler as *const () as u64));

    // Double fault with IST stack
    unsafe {
        idt.double_fault
            .set_handler_addr(x86_64::VirtAddr::new(entry_double_fault_handler as *const () as u64))
            .set_stack_index(crate::arch::gdt::DOUBLE_FAULT_IST_INDEX);
    }

});

/// Initialize the IDT — call once at boot after GDT is initialized.
pub fn init() {
    Lazy::force(&IDT);
    unsafe { (&*core::ptr::addr_of!(IDT_TABLE)).load(); }
}

/// Load the shared IDT on an application processor.
pub fn init_ap() {
    Lazy::force(&IDT);
    unsafe { (&*core::ptr::addr_of!(IDT_TABLE)).load(); }
    enable_alignment_checks();
}

fn enable_alignment_checks() {
    unsafe {
        let cr0: u64;
        core::arch::asm!("mov {}, cr0", out(reg) cr0, options(nomem, nostack));
        core::arch::asm!("mov cr0, {}", in(reg) cr0 | (1 << 18), options(nostack));
    }
}
