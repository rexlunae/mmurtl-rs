//! Interrupt Descriptor Table — Handles CPU exceptions and hardware IRQs.
//!
//! Sets up IDT entries for all CPU exceptions and hardware IRQs.
//! Includes handlers with proper x86-interrupt calling convention.

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
    if crate::apic::enabled() {
        crate::apic::eoi();
    } else {
        unsafe { eoi(irq) };
    }
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
// All handlers use `extern "x86-interrupt"` calling convention
// ========================================================================

extern "x86-interrupt" fn divide_error_handler(stack_frame: InterruptStackFrame) {
    panic!("DIVIDE ERROR\n{:#?}", stack_frame);
}

extern "x86-interrupt" fn debug_handler(stack_frame: InterruptStackFrame) {
    panic!("DEBUG\n{:#?}", stack_frame);
}

extern "x86-interrupt" fn nmi_handler(_stack_frame: InterruptStackFrame) {
    crate::serial::write_line("[NMI] Non-maskable interrupt received");
}

extern "x86-interrupt" fn breakpoint_handler(stack_frame: InterruptStackFrame) {
    crate::serial::write_str("[BREAKPOINT] at ");
    crate::serial::write_hex(stack_frame.instruction_pointer.as_u64());
    crate::serial::write_str("\n");
}

extern "x86-interrupt" fn overflow_handler(stack_frame: InterruptStackFrame) {
    panic!("OVERFLOW\n{:#?}", stack_frame);
}

extern "x86-interrupt" fn bound_range_handler(stack_frame: InterruptStackFrame) {
    panic!("BOUND RANGE EXCEEDED\n{:#?}", stack_frame);
}

extern "x86-interrupt" fn invalid_opcode_handler(stack_frame: InterruptStackFrame) {
    panic!("INVALID OPCODE\n{:#?}", stack_frame);
}

extern "x86-interrupt" fn device_not_available_handler(_stack_frame: InterruptStackFrame) {
    crate::serial::write_line("[#NM] Device not available — FPU task switch needed");
}

extern "x86-interrupt" fn double_fault_handler(
    stack_frame: InterruptStackFrame,
    _error_code: u64,
) -> ! {
    panic!("DOUBLE FAULT\n{:#?}", stack_frame);
}

extern "x86-interrupt" fn invalid_tss_handler(
    stack_frame: InterruptStackFrame,
    error_code: u64,
) {
    panic!("INVALID TSS error_code={:#x}\n{:#?}", error_code, stack_frame);
}

extern "x86-interrupt" fn segment_not_present_handler(
    stack_frame: InterruptStackFrame,
    error_code: u64,
) {
    panic!("SEGMENT NOT PRESENT error_code={:#x}\n{:#?}", error_code, stack_frame);
}

extern "x86-interrupt" fn stack_segment_handler(
    stack_frame: InterruptStackFrame,
    error_code: u64,
) {
    panic!("STACK SEGMENT FAULT error_code={:#x}\n{:#?}", error_code, stack_frame);
}

extern "x86-interrupt" fn general_protection_handler(
    stack_frame: InterruptStackFrame,
    error_code: u64,
) {
    panic!("GENERAL PROTECTION FAULT error_code={:#x}\n{:#?}", error_code, stack_frame);
}

extern "x86-interrupt" fn page_fault_handler(
    _stack_frame: InterruptStackFrame,
    error_code: PageFaultErrorCode,
) {
    use x86_64::registers::control::Cr2;
    let address = Cr2::read();

    crate::serial::write_str("\n[PAGE FAULT]\n");
    crate::serial::write_str("  Address: 0x");
    crate::serial::write_hex(address.as_u64());
    crate::serial::write_str("\n");
    crate::serial::write_str("  Error code:");
    crate::serial::write_hex_byte(error_code.bits() as u8);
    crate::serial::write_str("\n");

    panic!("Unresolved page fault");
}

extern "x86-interrupt" fn x87_fpu_handler(stack_frame: InterruptStackFrame) {
    panic!("x87 FPU ERROR\n{:#?}", stack_frame);
}

extern "x86-interrupt" fn alignment_check_handler(
    stack_frame: InterruptStackFrame,
    error_code: u64,
) {
    panic!("ALIGNMENT CHECK error_code={:#x}\n{:#?}", error_code, stack_frame);
}

extern "x86-interrupt" fn machine_check_handler(stack_frame: InterruptStackFrame) -> ! {
    panic!("MACHINE CHECK (hardware error)\n{:#?}", stack_frame);
}

extern "x86-interrupt" fn simd_fpu_handler(stack_frame: InterruptStackFrame) {
    panic!("SIMD FPU ERROR\n{:#?}", stack_frame);
}

extern "x86-interrupt" fn virtualization_handler(stack_frame: InterruptStackFrame) {
    panic!("VIRTUALIZATION EXCEPTION\n{:#?}", stack_frame);
}

extern "x86-interrupt" fn security_exception_handler(
    stack_frame: InterruptStackFrame,
    error_code: u64,
) {
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
    // Call the Rust scheduler — returns new RSP in RAX.
    // (EOI is sent inside schedule_and_switch, APIC- or PIC-appropriate.)
    // NOTE: schedule_and_switch returns with the scheduler lock still
    // held, so no other CPU can resume the outgoing task while we are
    // still executing on its stack.
    "call schedule_and_switch",
    // Switch to the new task's stack
    "mov rsp, rax",
    // Now that we're off the old task's stack, release the scheduler lock
    // (clobbers caller-saved registers — fine, we restore from the stack)
    "call scheduler_unlock",
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
    "iretq"
);

extern "x86-interrupt" fn keyboard_handler(_stack_frame: InterruptStackFrame) {
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
extern "x86-interrupt" fn spurious_handler(_stack_frame: InterruptStackFrame) {}

/// LAPIC error interrupt
extern "x86-interrupt" fn apic_error_handler(_stack_frame: InterruptStackFrame) {
    crate::serial::write_line("[APIC] Error interrupt received");
    crate::apic::eoi();
}

// ========================================================================
// IDT Initialization
// ========================================================================

/// Timer handler — defined in global_asm above
extern "C" {
    fn timer_handler();
}

/// The IDT — initialized once at boot
static IDT: Lazy<InterruptDescriptorTable> = Lazy::new(|| {
    let mut idt = InterruptDescriptorTable::new();

    // CPU exceptions
    idt.divide_error.set_handler_fn(divide_error_handler);
    idt.debug.set_handler_fn(debug_handler);
    idt.non_maskable_interrupt.set_handler_fn(nmi_handler);
    idt.breakpoint.set_handler_fn(breakpoint_handler);
    idt.overflow.set_handler_fn(overflow_handler);
    idt.bound_range_exceeded.set_handler_fn(bound_range_handler);
    idt.invalid_opcode.set_handler_fn(invalid_opcode_handler);
    idt.device_not_available.set_handler_fn(device_not_available_handler);
    idt.invalid_tss.set_handler_fn(invalid_tss_handler);
    idt.segment_not_present.set_handler_fn(segment_not_present_handler);
    idt.stack_segment_fault.set_handler_fn(stack_segment_handler);
    idt.general_protection_fault.set_handler_fn(general_protection_handler);
    idt.page_fault.set_handler_fn(page_fault_handler);
    idt.x87_floating_point.set_handler_fn(x87_fpu_handler);
    idt.alignment_check.set_handler_fn(alignment_check_handler);
    idt.machine_check.set_handler_fn(machine_check_handler);
    idt.simd_floating_point.set_handler_fn(simd_fpu_handler);
    idt.virtualization.set_handler_fn(virtualization_handler);
    idt.security_exception.set_handler_fn(security_exception_handler);

    // Hardware interrupts
    unsafe {
        idt[InterruptIndex::Timer.as_usize()].set_handler_addr(
            x86_64::VirtAddr::new(timer_handler as usize as u64)
        );
        // Reschedule IPI — identical save/schedule/restore flow
        idt[crate::apic::RESCHED_VECTOR as usize].set_handler_addr(
            x86_64::VirtAddr::new(timer_handler as usize as u64)
        );
    }
    idt[InterruptIndex::Keyboard.as_usize()].set_handler_fn(keyboard_handler);

    // LAPIC vectors
    idt[crate::apic::ERROR_VECTOR as usize].set_handler_fn(apic_error_handler);
    idt[crate::apic::SPURIOUS_VECTOR as usize].set_handler_fn(spurious_handler);

    // Double fault with IST stack
    unsafe {
        idt.double_fault
            .set_handler_fn(double_fault_handler)
            .set_stack_index(crate::gdt::DOUBLE_FAULT_IST_INDEX);
    }

    idt
});

/// Initialize the IDT — call once at boot after GDT is initialized.
pub fn init() {
    IDT.load();
}

/// Load the shared IDT on an application processor.
pub fn init_ap() {
    IDT.load();
}
