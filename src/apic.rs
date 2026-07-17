//! Local APIC + I/O APIC driver.
//!
//! Replaces the legacy 8259 PIC / PIT combo on modern hardware:
//!   - The Local APIC's timer (calibrated against the PIT) drives the
//!     scheduler tick on the BSP.
//!   - The I/O APIC routes legacy IRQs (keyboard) to the BSP.
//!   - The Local APIC's ICR sends INIT/SIPI IPIs to boot the APs.
//!
//! Uses xAPIC MMIO mode (base typically 0xFEE0_0000), accessed through the
//! physical-memory offset mapping.

use core::sync::atomic::{AtomicBool, AtomicU32, AtomicU64, Ordering};
use x86_64::instructions::port::Port;
use x86_64::PhysAddr;

// ========================================================================
// Interrupt vector assignments
// ========================================================================

/// LAPIC timer → same vector the PIT used, so the context-switch handler
/// in interrupts.rs works unchanged.
pub const TIMER_VECTOR: u8 = crate::interrupts::PIC_1_OFFSET;
/// Keyboard, routed through the I/O APIC (GSI 1 on QEMU/ISA)
pub const KEYBOARD_VECTOR: u8 = crate::interrupts::PIC_1_OFFSET + 1;
/// Reschedule IPI — kicks a CPU into the scheduler immediately
pub const RESCHED_VECTOR: u8 = 0x30;
/// LAPIC error interrupt
pub const ERROR_VECTOR: u8 = 0xFE;
/// Spurious interrupt vector (low nibble must be 0xF on some CPUs)
pub const SPURIOUS_VECTOR: u8 = 0xFF;

// ========================================================================
// Local APIC register offsets (xAPIC MMIO)
// ========================================================================

const LAPIC_ID: u32 = 0x020;
const LAPIC_VERSION: u32 = 0x030;
const LAPIC_TPR: u32 = 0x080;
const LAPIC_EOI: u32 = 0x0B0;
const LAPIC_SVR: u32 = 0x0F0;
const LAPIC_ESR: u32 = 0x280;
const LAPIC_ICR_LOW: u32 = 0x300;
const LAPIC_ICR_HIGH: u32 = 0x310;
const LAPIC_LVT_TIMER: u32 = 0x320;
const LAPIC_LVT_LINT0: u32 = 0x350;
const LAPIC_LVT_LINT1: u32 = 0x360;
const LAPIC_LVT_ERROR: u32 = 0x370;
const LAPIC_TIMER_INIT_COUNT: u32 = 0x380;
const LAPIC_TIMER_CUR_COUNT: u32 = 0x390;
const LAPIC_TIMER_DIVIDE: u32 = 0x3E0;

const LVT_MASKED: u32 = 1 << 16;
const TIMER_PERIODIC: u32 = 1 << 17;
/// Divide-by-16 configuration for the timer
const TIMER_DIVIDE_BY_16: u32 = 0b0011;

/// Virtual address of the LAPIC MMIO window (0 = not initialized)
static LAPIC_VIRT: AtomicU64 = AtomicU64::new(0);
/// True once the LAPIC is enabled and driving interrupts
static APIC_ENABLED: AtomicBool = AtomicBool::new(false);
/// LAPIC timer ticks per 10 ms (divide-by-16), measured once on the BSP
static TIMER_TICKS_PER_10MS: AtomicU32 = AtomicU32::new(0);

/// Whether APIC mode is active (LAPIC drives the timer, PIC is masked)
pub fn enabled() -> bool {
    APIC_ENABLED.load(Ordering::Acquire)
}

// ========================================================================
// Register access
// ========================================================================

unsafe fn reg_read(offset: u32) -> u32 {
    let base = LAPIC_VIRT.load(Ordering::Acquire);
    core::ptr::read_volatile((base + offset as u64) as *const u32)
}

unsafe fn reg_write(offset: u32, value: u32) {
    let base = LAPIC_VIRT.load(Ordering::Acquire);
    core::ptr::write_volatile((base + offset as u64) as *mut u32, value);
}

/// This CPU's Local APIC ID
pub fn local_apic_id() -> u32 {
    unsafe { reg_read(LAPIC_ID) >> 24 }
}

/// Signal end-of-interrupt to the Local APIC
pub fn eoi() {
    unsafe { reg_write(LAPIC_EOI, 0) }
}

// ========================================================================
// Initialization
// ========================================================================

/// Initialize the Local APIC on the BSP: map MMIO, enable it, route the
/// keyboard through the I/O APIC, and calibrate the timer.
///
/// Returns false (leaving the kernel in PIC/PIT mode) if no MADT was found.
pub fn init() -> bool {
    let madt = match crate::acpi::madt() {
        Some(m) => m,
        None => {
            crate::serial::write_line("[APIC] No MADT — staying on PIC/PIT");
            return false;
        }
    };

    // Prefer the base from the IA32_APIC_BASE MSR (authoritative), and make
    // sure the global enable bit is set.
    let mut apic_base_msr = x86_64::registers::model_specific::Msr::new(0x1B);
    let msr_val = unsafe { apic_base_msr.read() };
    let base = if msr_val & 0xF_FFFF_F000 != 0 {
        msr_val & 0xF_FFFF_F000
    } else {
        madt.lapic_base
    };
    if msr_val & (1 << 11) == 0 {
        unsafe { apic_base_msr.write(msr_val | (1 << 11)) };
    }

    // Map the LAPIC MMIO page (uncached) through the phys-offset window
    crate::memory::ensure_phys_mapped(base, 0x1000, true);
    let virt = crate::memory::page_table::phys_to_virt(PhysAddr::new(base));
    LAPIC_VIRT.store(virt.as_u64(), Ordering::Release);

    crate::serial::write_str("[APIC] LAPIC base 0x");
    crate::serial::write_hex(base);
    crate::serial::write_str(" (version 0x");
    crate::serial::write_hex_byte(unsafe { reg_read(LAPIC_VERSION) } as u8);
    crate::serial::write_str("), BSP APIC ID ");

    enable_current_cpu();
    crate::serial::write_dec(local_apic_id() as u64);
    crate::serial::write_str("\n");

    // Route the keyboard IRQ through the I/O APIC to the BSP
    if let Some(ioapic_base) = madt.ioapic_base {
        let (gsi, flags) = madt.isa_irq_to_gsi(1);
        ioapic_route(ioapic_base, madt.ioapic_gsi_base, gsi, flags, KEYBOARD_VECTOR, local_apic_id());
    }

    // Fully mask the legacy PIC — the LAPIC/IOAPIC own interrupts now
    crate::interrupts::mask_pic();

    calibrate_timer();

    APIC_ENABLED.store(true, Ordering::Release);
    true
}

/// Enable the Local APIC on the calling CPU (BSP and APs).
///
/// Sets the spurious vector + software-enable bit, accepts all interrupt
/// priorities, and masks the local LVT entries until they're needed.
pub fn enable_current_cpu() {
    unsafe {
        // Software-enable + spurious vector
        reg_write(LAPIC_SVR, 0x100 | SPURIOUS_VECTOR as u32);
        // Accept all interrupt priorities
        reg_write(LAPIC_TPR, 0);
        // Mask local interrupt pins and the timer until configured
        reg_write(LAPIC_LVT_TIMER, LVT_MASKED);
        reg_write(LAPIC_LVT_LINT0, LVT_MASKED);
        reg_write(LAPIC_LVT_LINT1, LVT_MASKED);
        reg_write(LAPIC_LVT_ERROR, ERROR_VECTOR as u32);
        // Clear any stale errors
        reg_write(LAPIC_ESR, 0);
        reg_write(LAPIC_ESR, 0);
    }
}

// ========================================================================
// LAPIC timer
// ========================================================================

/// Measure LAPIC timer ticks per 10 ms against the PIT (channel 2 one-shot)
fn calibrate_timer() {
    unsafe {
        reg_write(LAPIC_TIMER_DIVIDE, TIMER_DIVIDE_BY_16);
        reg_write(LAPIC_LVT_TIMER, LVT_MASKED); // count without interrupting
        reg_write(LAPIC_TIMER_INIT_COUNT, 0xFFFF_FFFF);

        pit_wait_ms(10);

        let elapsed = 0xFFFF_FFFFu32 - reg_read(LAPIC_TIMER_CUR_COUNT);
        reg_write(LAPIC_TIMER_INIT_COUNT, 0); // stop

        TIMER_TICKS_PER_10MS.store(elapsed, Ordering::Release);

        crate::serial::write_str("[APIC] Timer calibrated: ");
        crate::serial::write_dec(elapsed as u64);
        crate::serial::write_str(" ticks / 10 ms (div 16)\n");
    }
}

/// Start the LAPIC timer in periodic mode at the given frequency on the
/// calling CPU, firing TIMER_VECTOR.
pub fn start_timer(hz: u32) {
    let per_10ms = TIMER_TICKS_PER_10MS.load(Ordering::Acquire) as u64;
    assert!(per_10ms > 0, "LAPIC timer not calibrated");
    let count = (per_10ms * 100 / hz as u64).max(1) as u32;

    unsafe {
        reg_write(LAPIC_TIMER_DIVIDE, TIMER_DIVIDE_BY_16);
        reg_write(LAPIC_LVT_TIMER, TIMER_PERIODIC | TIMER_VECTOR as u32);
        reg_write(LAPIC_TIMER_INIT_COUNT, count);
    }

    crate::serial::write_str("[APIC] LAPIC timer started: ");
    crate::serial::write_dec(hz as u64);
    crate::serial::write_str(" Hz (count=");
    crate::serial::write_dec(count as u64);
    crate::serial::write_str(")\n");
}

// ========================================================================
// PIT-based busy-wait (channel 2, mode 0 one-shot)
// ========================================================================

/// Busy-wait using PIT channel 2 as a one-shot countdown.
///
/// Channel 2 is the PC speaker channel: its gate is controlled by port 0x61
/// bit 0 and its output is readable at port 0x61 bit 5, so it can be used
/// for timing without generating interrupts. Max ~54 ms per call.
pub fn pit_wait_ms(ms: u32) {
    const PIT_HZ: u64 = 1_193_182;
    let ticks = (PIT_HZ * ms as u64 / 1000).min(0xFFFF) as u16;

    unsafe {
        let mut port_61: Port<u8> = Port::new(0x61);
        let mut cmd: Port<u8> = Port::new(0x43);
        let mut ch2: Port<u8> = Port::new(0x42);

        // Gate low (stop counter), speaker off
        let val = port_61.read() & !0b11;
        port_61.write(val);

        // Channel 2, lobyte/hibyte, mode 0 (interrupt on terminal count)
        cmd.write(0b1011_0000u8);
        ch2.write((ticks & 0xFF) as u8);
        ch2.write((ticks >> 8) as u8);

        // Gate high — counter starts, OUT goes low until terminal count
        port_61.write(val | 0b01);

        // Wait for OUT (bit 5) to go high
        while port_61.read() & (1 << 5) == 0 {
            core::hint::spin_loop();
        }

        // Gate low again
        port_61.write(val);
    }
}

// ========================================================================
// Inter-Processor Interrupts (ICR)
// ========================================================================

/// Wait for a previous IPI to be delivered (ICR delivery status bit clear)
unsafe fn icr_wait() {
    while reg_read(LAPIC_ICR_LOW) & (1 << 12) != 0 {
        core::hint::spin_loop();
    }
}

/// Send an INIT IPI to the target APIC ID
pub fn send_init(apic_id: u32) {
    unsafe {
        reg_write(LAPIC_ICR_HIGH, apic_id << 24);
        // Delivery mode INIT (101), level assert, edge trigger
        reg_write(LAPIC_ICR_LOW, 0x0000_4500);
        icr_wait();
    }
}

/// Send a fixed-delivery IPI to the target APIC ID
pub fn send_ipi(apic_id: u32, vector: u8) {
    unsafe {
        reg_write(LAPIC_ICR_HIGH, apic_id << 24);
        // Fixed delivery, physical destination, assert
        reg_write(LAPIC_ICR_LOW, 0x0000_4000 | vector as u32);
        icr_wait();
    }
}

/// Send a Startup IPI to the target APIC ID.
///
/// The AP begins real-mode execution at physical address `vector << 12`.
pub fn send_sipi(apic_id: u32, vector: u8) {
    unsafe {
        reg_write(LAPIC_ICR_HIGH, apic_id << 24);
        // Delivery mode STARTUP (110)
        reg_write(LAPIC_ICR_LOW, 0x0000_4600 | vector as u32);
        icr_wait();
    }
}

// ========================================================================
// I/O APIC
// ========================================================================

/// Program an I/O APIC redirection entry to route a GSI to a CPU
fn ioapic_route(
    ioapic_phys: u64,
    gsi_base: u32,
    gsi: u32,
    mps_flags: u16,
    vector: u8,
    dest_apic_id: u32,
) {
    if gsi < gsi_base {
        return;
    }
    let index = gsi - gsi_base;

    crate::memory::ensure_phys_mapped(ioapic_phys, 0x1000, true);
    let virt = crate::memory::page_table::phys_to_virt(PhysAddr::new(ioapic_phys)).as_u64();

    let regsel = virt as *mut u32;
    let regwin = (virt + 0x10) as *mut u32;

    let mut write_reg = |reg: u32, val: u32| unsafe {
        core::ptr::write_volatile(regsel, reg);
        core::ptr::write_volatile(regwin, val);
    };

    // MPS INTI flags: polarity (bits 0-1): 11 = active low
    //                 trigger (bits 2-3): 11 = level
    let active_low = mps_flags & 0b11 == 0b11;
    let level_triggered = (mps_flags >> 2) & 0b11 == 0b11;

    let mut low = vector as u32; // fixed delivery, physical destination
    if active_low {
        low |= 1 << 13;
    }
    if level_triggered {
        low |= 1 << 15;
    }

    // High dword first (destination), then low (unmasks the entry)
    write_reg(0x10 + index * 2 + 1, dest_apic_id << 24);
    write_reg(0x10 + index * 2, low);

    crate::serial::write_str("[IOAPIC] GSI ");
    crate::serial::write_dec(gsi as u64);
    crate::serial::write_str(" -> vector ");
    crate::serial::write_dec(vector as u64);
    crate::serial::write_str(" on APIC ID ");
    crate::serial::write_dec(dest_apic_id as u64);
    crate::serial::write_str("\n");
}
