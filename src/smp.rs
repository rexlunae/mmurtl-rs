//! SMP — Application Processor (AP) boot via INIT-SIPI-SIPI.
//!
//! APs start in 16-bit real mode at a physical address chosen by the SIPI
//! vector. We place a trampoline at 0x8000 that walks each AP up the mode
//! ladder — real mode → protected mode → long mode with the kernel's page
//! tables — and then jumps into `ap_entry()` in Rust on a per-CPU stack.
//!
//! Boot sequence per AP (one at a time, mailbox handshake):
//!   1. Patch the trampoline mailbox: kernel CR3, stack top, entry, CPU number
//!   2. INIT IPI, wait 10 ms
//!   3. STARTUP IPI (vector 0x08 → real-mode entry at 0x8000)
//!   4. Wait for the AP to set the ready flag (retry SIPI once if needed)

use alloc::boxed::Box;
use core::sync::atomic::{AtomicBool, AtomicUsize, Ordering};

/// Physical address of the AP trampoline (must be 4K-aligned, below 1 MiB)
pub const TRAMPOLINE_PHYS: u64 = 0x8000;

/// Stack size for each AP's boot stack
pub const AP_STACK_SIZE: usize = 64 * 1024;

/// Number of CPUs that have completed bring-up (BSP counts as 1)
static CPUS_ONLINE: AtomicUsize = AtomicUsize::new(1);

/// Handshake flag: set by an AP once it is fully online
static AP_READY: AtomicBool = AtomicBool::new(false);

pub fn cpus_online() -> usize {
    CPUS_ONLINE.load(Ordering::Acquire)
}

// ========================================================================
// The trampoline
//
// Assembled into the kernel image and copied to TRAMPOLINE_PHYS at runtime.
// All absolute references are computed as `label - ap_trampoline_start +
// 0x8000`, so the code is correct at its copied location regardless of
// where the linker put it. The mailbox words at the end are patched by the
// BSP before each SIPI.
// ========================================================================

core::arch::global_asm!(
    r#"
.section .text.ap_trampoline, "ax"
.code16
.global ap_trampoline_start
ap_trampoline_start:
    cli
    cld
    xorw %ax, %ax
    movw %ax, %ds
    movw %ax, %es
    movw %ax, %ss

    lgdtl ap_tramp_gdt_ptr - ap_trampoline_start + 0x8000

    # Protected mode
    movl %cr0, %eax
    orl $1, %eax
    movl %eax, %cr0
    ljmpl $0x08, $(ap_tramp_pm32 - ap_trampoline_start + 0x8000)

.code32
ap_tramp_pm32:
    movw $0x10, %ax
    movw %ax, %ds
    movw %ax, %es
    movw %ax, %ss
    movw %ax, %fs
    movw %ax, %gs

    # Enable PAE
    movl %cr4, %eax
    orl $(1 << 5), %eax
    movl %eax, %cr4

    # Adopt the kernel's page tables
    movl ap_tramp_cr3 - ap_trampoline_start + 0x8000, %eax
    movl %eax, %cr3

    # EFER: long mode enable + NX enable (kernel PTEs carry the NX bit)
    movl $0xC0000080, %ecx
    rdmsr
    orl $((1 << 8) | (1 << 11)), %eax
    wrmsr

    # Enable paging + write protect -> long mode active
    movl %cr0, %eax
    orl $0x80010000, %eax
    movl %eax, %cr0
    ljmpl $0x18, $(ap_tramp_lm64 - ap_trampoline_start + 0x8000)

.code64
ap_tramp_lm64:
    movw $0x10, %ax
    movw %ax, %ds
    movw %ax, %es
    movw %ax, %ss
    xorl %eax, %eax
    movw %ax, %fs
    movw %ax, %gs

    # Per-CPU stack, CPU number argument, and Rust entry point
    movq ap_tramp_stack - ap_trampoline_start + 0x8000, %rsp
    movq ap_tramp_cpu_num - ap_trampoline_start + 0x8000, %rdi
    movq ap_tramp_entry - ap_trampoline_start + 0x8000, %rax
    xorl %ebp, %ebp
    jmpq *%rax

# Minimal GDT for the mode transitions (the AP loads the real kernel GDT
# once it reaches Rust code)
.balign 16
ap_tramp_gdt:
    .quad 0                     # null
    .quad 0x00CF9A000000FFFF    # 0x08: 32-bit code, base 0, limit 4G
    .quad 0x00CF92000000FFFF    # 0x10: data
    .quad 0x00AF9A000000FFFF    # 0x18: 64-bit code
ap_tramp_gdt_ptr:
    .word ap_tramp_gdt_ptr - ap_tramp_gdt - 1
    .long ap_tramp_gdt - ap_trampoline_start + 0x8000

# Mailbox — patched by the BSP before each SIPI
.balign 8
.global ap_tramp_cr3
ap_tramp_cr3:     .quad 0
.global ap_tramp_stack
ap_tramp_stack:   .quad 0
.global ap_tramp_entry
ap_tramp_entry:   .quad 0
.global ap_tramp_cpu_num
ap_tramp_cpu_num: .quad 0

.global ap_trampoline_end
ap_trampoline_end:

.text
.code64
"#,
    options(att_syntax)
);

extern "C" {
    static ap_trampoline_start: u8;
    static ap_trampoline_end: u8;
    static ap_tramp_cr3: u8;
    static ap_tramp_stack: u8;
    static ap_tramp_entry: u8;
    static ap_tramp_cpu_num: u8;
}

/// Virtual address (via phys offset mapping) of a trampoline field at its
/// copied location
unsafe fn tramp_field_ptr(field: *const u8) -> *mut u64 {
    let start = &ap_trampoline_start as *const u8 as u64;
    let offset = field as u64 - start;
    let virt = crate::memory::page_table::phys_to_virt(
        x86_64::PhysAddr::new(TRAMPOLINE_PHYS + offset),
    );
    virt.as_mut_ptr()
}

/// Copy the trampoline to TRAMPOLINE_PHYS and fill in the kernel CR3
unsafe fn install_trampoline() {
    let start = &ap_trampoline_start as *const u8;
    let end = &ap_trampoline_end as *const u8;
    let len = end as usize - start as usize;
    assert!(len <= 4096, "AP trampoline larger than one page");

    // The AP enables paging while executing at 0x8000, so the kernel page
    // tables must identity-map that page (and it must be executable).
    crate::memory::identity_map_executable(TRAMPOLINE_PHYS);

    let dst = crate::memory::page_table::phys_to_virt(
        x86_64::PhysAddr::new(TRAMPOLINE_PHYS),
    )
    .as_mut_ptr::<u8>();
    core::ptr::copy_nonoverlapping(start, dst, len);

    // All APs share the kernel's address space
    let (pml4, _) = x86_64::registers::control::Cr3::read();
    tramp_field_ptr(&ap_tramp_cr3).write_volatile(pml4.start_address().as_u64());

    crate::serial::write_str("[SMP] Trampoline installed at 0x");
    crate::serial::write_hex(TRAMPOLINE_PHYS);
    crate::serial::write_str(" (");
    crate::serial::write_dec(len as u64);
    crate::serial::write_str(" bytes)\n");
}

// ========================================================================
// AP bring-up
// ========================================================================

/// Boot all application processors listed in the MADT.
///
/// Must be called with interrupts disabled, after APIC + heap init.
pub fn boot_aps() {
    if !crate::apic::enabled() {
        crate::serial::write_line("[SMP] APIC not enabled — cannot boot APs");
        return;
    }
    let madt = match crate::acpi::madt() {
        Some(m) => m,
        None => return,
    };

    let bsp_id = crate::apic::local_apic_id();
    let ap_count = madt
        .cpu_apic_ids
        .iter()
        .filter(|&&id| id != bsp_id)
        .count();

    if ap_count == 0 {
        crate::serial::write_line("[SMP] Single CPU system — no APs to boot");
        return;
    }

    crate::serial::write_str("[SMP] Booting ");
    crate::serial::write_dec(ap_count as u64);
    crate::serial::write_str(" AP(s)...\n");

    unsafe {
        install_trampoline();
    }

    let mut cpu_num = 1u64; // BSP is CPU 0
    for &apic_id in madt.cpu_apic_ids.iter() {
        if apic_id == bsp_id {
            continue;
        }
        boot_one_ap(apic_id, cpu_num);
        cpu_num += 1;
    }

    crate::serial::write_str("[SMP] ");
    crate::serial::write_dec(cpus_online() as u64);
    crate::serial::write_str(" CPU(s) online\n");
}

/// Boot a single AP and wait for it to report in
fn boot_one_ap(apic_id: u32, cpu_num: u64) {
    // Per-CPU boot stack, leaked: the AP keeps using it forever
    let stack: Box<[u8]> = alloc::vec![0u8; AP_STACK_SIZE].into_boxed_slice();
    let stack = Box::leak(stack);
    let stack_top = (stack.as_ptr() as u64 + stack.len() as u64) & !0xF;

    unsafe {
        tramp_field_ptr(&ap_tramp_stack).write_volatile(stack_top);
        tramp_field_ptr(&ap_tramp_entry).write_volatile(ap_entry as u64);
        tramp_field_ptr(&ap_tramp_cpu_num).write_volatile(cpu_num);
    }

    AP_READY.store(false, Ordering::SeqCst);

    crate::serial::write_str("[SMP] Starting CPU ");
    crate::serial::write_dec(cpu_num);
    crate::serial::write_str(" (APIC ID ");
    crate::serial::write_dec(apic_id as u64);
    crate::serial::write_str(")...\n");

    // INIT — put the AP into wait-for-SIPI state
    crate::apic::send_init(apic_id);
    crate::apic::pit_wait_ms(10);

    // First SIPI: real-mode entry at (vector << 12) = 0x8000
    let sipi_vector = (TRAMPOLINE_PHYS >> 12) as u8;
    crate::apic::send_sipi(apic_id, sipi_vector);

    if wait_for_ap(20) {
        return;
    }

    // Slow starter — the spec says send a second SIPI
    crate::apic::send_sipi(apic_id, sipi_vector);
    if !wait_for_ap(1000) {
        crate::serial::write_str("[SMP] CPU ");
        crate::serial::write_dec(cpu_num);
        crate::serial::write_str(" (APIC ID ");
        crate::serial::write_dec(apic_id as u64);
        crate::serial::write_str(") failed to start!\n");
    }
}

/// Wait up to `timeout_ms` for the AP ready flag
fn wait_for_ap(timeout_ms: u32) -> bool {
    for _ in 0..timeout_ms {
        if AP_READY.load(Ordering::SeqCst) {
            return true;
        }
        crate::apic::pit_wait_ms(1);
    }
    AP_READY.load(Ordering::SeqCst)
}

// ========================================================================
// AP entry point (arrives here from the trampoline, in long mode)
// ========================================================================

/// First Rust code an AP executes. `cpu_num` comes from the mailbox.
#[no_mangle]
pub extern "C" fn ap_entry(cpu_num: u64) -> ! {
    // Adopt the kernel's real GDT and IDT, then enable this CPU's LAPIC
    crate::gdt::init_ap();
    crate::interrupts::init_ap();
    crate::apic::enable_current_cpu();

    let apic_id = crate::apic::local_apic_id();
    CPUS_ONLINE.fetch_add(1, Ordering::SeqCst);

    crate::serial::write_str("[SMP] CPU ");
    crate::serial::write_dec(cpu_num);
    crate::serial::write_str(" online (APIC ID ");
    crate::serial::write_dec(apic_id as u64);
    crate::serial::write_str(")\n");

    // Signal the BSP that bring-up is complete
    AP_READY.store(true, Ordering::SeqCst);

    // Park with interrupts enabled — ready to service future IPIs. The
    // LAPIC timer on this CPU stays masked until the scheduler goes SMP.
    x86_64::instructions::interrupts::enable();
    loop {
        x86_64::instructions::hlt();
    }
}
