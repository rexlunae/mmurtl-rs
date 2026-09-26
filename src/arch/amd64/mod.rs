//! amd64 (x86_64 long mode) port.
//!
//! Boot via the `bootloader` crate (BIOS or UEFI); 16550 serial; GDT/TSS
//! and IDT; 8259 PIC → Local APIC + I/O APIC; ACPI MADT for CPU discovery;
//! INIT-SIPI-SIPI multi-core boot; PCI enumeration with legacy virtio-pci
//! and an xHCI skeleton; ring-3 userspace via an `int 0x80` gate.

pub mod acpi;
pub mod apic;
pub mod boot;
pub mod context;
pub mod gdt;
pub mod interrupts;
pub mod memory;
pub mod page_table;
pub mod serial;
pub mod smp;
pub mod syscall;
pub mod usb;
pub mod user_programs;

use core::sync::atomic::{AtomicU32, Ordering};

pub use context::{kernel_context, on_switch_to_user, user_context, yield_now, TaskContext};
pub use memory::{
    free_address_space, heap_extend, heap_init, map_user_page, new_address_space, phys_to_virt,
    query_page, switch_address_space, user_access_begin, user_access_end,
};

/// Architecture name for the boot log
pub const NAME: &str = "amd64";

// ========================================================================
// PCI configuration space + I/O ports
// ========================================================================

/// PCI config read through CONFIG_ADDRESS/CONFIG_DATA (0xCF8/0xCFC)
pub fn pci_config_read(bus: u8, device: u8, function: u8, register: u8) -> u32 {
    use x86_64::instructions::port::Port;
    let addr = 0x8000_0000u32
        | (bus as u32) << 16
        | (device as u32) << 11
        | (function as u32) << 8
        | register as u32;
    without_interrupts(|| unsafe {
        Port::<u32>::new(0xCF8).write(addr);
        Port::<u32>::new(0xCFC).read()
    })
}

/// PCI config write through 0xCF8/0xCFC
pub fn pci_config_write(bus: u8, device: u8, function: u8, register: u8, value: u32) {
    use x86_64::instructions::port::Port;
    let addr = 0x8000_0000u32
        | (bus as u32) << 16
        | (device as u32) << 11
        | (function as u32) << 8
        | register as u32;
    without_interrupts(|| unsafe {
        Port::<u32>::new(0xCF8).write(addr);
        Port::<u32>::new(0xCFC).write(value);
    })
}

pub fn io_read8(port: u16) -> u8 {
    unsafe { x86_64::instructions::port::Port::<u8>::new(port).read() }
}
pub fn io_read16(port: u16) -> u16 {
    unsafe { x86_64::instructions::port::Port::<u16>::new(port).read() }
}
pub fn io_read32(port: u16) -> u32 {
    unsafe { x86_64::instructions::port::Port::<u32>::new(port).read() }
}
pub fn io_write8(port: u16, v: u8) {
    unsafe { x86_64::instructions::port::Port::<u8>::new(port).write(v) }
}
pub fn io_write16(port: u16, v: u16) {
    unsafe { x86_64::instructions::port::Port::<u16>::new(port).write(v) }
}
pub fn io_write32(port: u16, v: u32) {
    unsafe { x86_64::instructions::port::Port::<u32>::new(port).write(v) }
}

/// Make freshly written code visible to instruction fetch: nothing to do
/// on x86, whose instruction caches snoop data writes
pub fn sync_icache(_kva: *const u8, _len: usize) {}

/// ELF e_machine for user programs (EM_X86_64)
pub const ELF_MACHINE: u16 = 0x3E;

/// How user code enters the kernel
pub const SYSCALL_MECHANISM: &str = "int 0x80 gate (DPL 3)";

// ========================================================================
// Interrupts
// ========================================================================

/// Run `f` with interrupts disabled on this CPU
pub fn without_interrupts<R>(f: impl FnOnce() -> R) -> R {
    x86_64::instructions::interrupts::without_interrupts(f)
}

/// Enable interrupts on this CPU
pub fn enable_interrupts() {
    x86_64::instructions::interrupts::enable();
}

/// Sleep until the next interrupt
pub fn halt() {
    x86_64::instructions::hlt();
}

// ========================================================================
// CPU identity + IPIs
// ========================================================================

/// APIC ID → CPU index, written at CPU registration, read lock-free on
/// every timer tick. Index by APIC ID (xAPIC IDs are < 256).
static APIC_TO_CPU: [AtomicU32; 256] = {
    const ZERO: AtomicU32 = AtomicU32::new(0);
    [ZERO; 256]
};

/// Scheduler index of the calling CPU
pub fn cpu_index() -> usize {
    if !apic::enabled() {
        return 0;
    }
    let apic_id = apic::local_apic_id() as usize;
    APIC_TO_CPU[apic_id & 0xFF].load(Ordering::Relaxed) as usize
}

/// Record the calling CPU's scheduler index (called on that CPU)
pub fn set_cpu_index(cpu: usize) {
    let id = hw_cpu_id() as usize;
    APIC_TO_CPU[id & 0xFF].store(cpu as u32, Ordering::Relaxed);
}

/// The calling CPU's Local APIC ID (0 without an APIC)
pub fn hw_cpu_id() -> u32 {
    if apic::enabled() {
        apic::local_apic_id()
    } else {
        0
    }
}

/// Whether reschedule IPIs can be sent
pub fn ipi_available() -> bool {
    apic::enabled()
}

/// Kick the CPU with Local APIC ID `hw_id` into its scheduler
pub fn send_resched_ipi(hw_id: u32) {
    apic::send_ipi(hw_id, apic::RESCHED_VECTOR);
}

/// Number of CPUs brought online
pub fn cpus_online() -> usize {
    smp::cpus_online()
}

// ========================================================================
// Time
// ========================================================================

/// Start the calling CPU's scheduler tick: the LAPIC timer in APIC mode,
/// or (boot CPU only) the PIT through the legacy PIC
pub fn start_tick(hz: u32, boot_cpu: bool) {
    if apic::enabled() {
        apic::start_timer(hz);
    } else if boot_cpu {
        init_pit(hz);
        unsafe {
            // Legacy mode: unmask the timer IRQ in the PIC
            let mut pic1_data: x86_64::instructions::port::Port<u8> =
                x86_64::instructions::port::Port::new(0x21);
            let mask = pic1_data.read();
            pic1_data.write(mask & !0x01);
        }
        crate::serial::write_str("[PIC] Timer IRQ0 unmasked\n");
    }
}

/// Program the PIT (8253) to fire at `hz` (fallback tick source when there
/// is no APIC)
fn init_pit(hz: u32) {
    use x86_64::instructions::port::Port;

    // PIT frequency: 1.193182 MHz base clock
    let divisor: u16 = (1193182u32 / hz) as u16;

    crate::serial::write_str("[PIT] Frequency: ");
    crate::serial::write_dec(hz as u64);
    crate::serial::write_str(" Hz (divisor=");
    crate::serial::write_dec(divisor as u64);
    crate::serial::write_str(")\n");

    unsafe {
        // Channel 0, lobyte/hibyte, mode 3 (square wave), binary mode
        let mut cmd_port: Port<u8> = Port::new(0x43);
        cmd_port.write(0x36u8);

        let mut data_port: Port<u8> = Port::new(0x40);
        data_port.write((divisor & 0xFF) as u8);
        data_port.write(((divisor >> 8) & 0xFF) as u8);
    }
}

/// Busy-wait for roughly `ms` milliseconds
pub fn delay_ms(ms: u32) {
    apic::pit_wait_ms(ms);
}
