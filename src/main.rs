//! MMURTL/RS — Message-passing multitasking real-time kernel in Rust
//!
//! A Rust port inspired by Richard Burgess's MMURTL kernel (1994).
//! Architecture: x86_64 long mode, UEFI boot, message-passing IPC.

#![no_std]
#![no_main]
#![feature(abi_x86_interrupt)]

extern crate alloc;

mod serial;
mod gdt;
mod interrupts;
mod memory;
mod scheduler;
mod ipc;
mod pci;
mod usb;
mod acpi;
mod apic;
mod smp;
mod virtio;
mod keyboard;
mod fs;

use bootloader_api::BootInfo;
use bootloader_api::info::Optional;
use core::panic::PanicInfo;

/// MMURTL/RS version info
pub const VERSION: &str = env!("CARGO_PKG_VERSION");
pub const KERNEL_NAME: &str = "MMURTL/RS";
pub const BOOT_BANNER: &str = include_str!("banner.txt");

/// Kernel entry point — called by bootloader
fn kernel_main(boot_info: &'static mut BootInfo) -> ! {
    // Initialize serial early for debugging
    serial::init();
    serial::write_str(KERNEL_NAME);
    serial::write_str(" v");
    serial::write_str(VERSION);
    serial::write_str(" booting...\n\n");

    // Print banner
    serial::write_str(BOOT_BANNER);
    serial::write_str("\n");

    // Log boot info — memory regions
    let region_count = boot_info.memory_regions.len();
    serial::write_str("[INFO] Memory regions: ");
    serial::write_dec(region_count as u64);
    serial::write_str("\n");

    // Log physical memory offset
    match boot_info.physical_memory_offset {
        Optional::Some(offset) => {
            serial::write_str("[INFO] Physical memory offset: 0x");
            serial::write_hex(offset);
            serial::write_str("\n");
        }
        Optional::None => {
            serial::write_str("[WARN] Physical memory not mapped by bootloader\n");
        }
    }

    // Count usable memory
    let mut total_usable: u64 = 0;
    for region in boot_info.memory_regions.iter() {
        use bootloader_api::info::MemoryRegionKind;
        if region.kind == MemoryRegionKind::Usable {
            total_usable += region.end - region.start;
        }
    }
    serial::write_str("[INFO] Usable memory: ");
    serial::write_dec(total_usable / (1024 * 1024));
    serial::write_str(" MiB\n");

    // Initialize CPU structures
    serial::write_str("[INIT] GDT...\n");
    gdt::init();

    serial::write_str("[INIT] IDT...\n");
    interrupts::init();

    // Initialize PIC (Programmable Interrupt Controller)
    serial::write_str("[INIT] PIC...\n");
    interrupts::init_pic();

    // Save the RSDP address before memory takes ownership of boot_info
    let rsdp_addr = match boot_info.rsdp_addr {
        Optional::Some(addr) => Some(addr),
        Optional::None => None,
    };

    // Initialize memory management
    serial::write_str("[INIT] Memory manager...\n");
    memory::init(boot_info);

    // Parse ACPI tables (MADT: CPUs, Local APIC, I/O APIC)
    serial::write_str("[INIT] ACPI...\n");
    acpi::init(rsdp_addr);

    // Switch from PIC/PIT to Local APIC + I/O APIC
    serial::write_str("[INIT] APIC...\n");
    apic::init();

    // Initialize the scheduler on the BSP (must precede AP boot: APs
    // register themselves and start their timers as they come up)
    serial::write_str("[INIT] Scheduler...\n");
    scheduler::init();

    // Boot the application processors — each joins the scheduler
    serial::write_str("[INIT] SMP...\n");
    smp::boot_aps();

    // Initialize PCI and USB
    serial::write_str("[INIT] PCI bus...\n");
    let devices = pci::scan();

    serial::write_str("[INIT] USB subsystem...\n");
    usb::init();

    // Virtio drivers: storage (virtio-blk) + network (virtio-net)
    serial::write_str("[INIT] Virtio drivers...\n");
    virtio::init(&devices);

    // Driver proof-of-life: block device write/read/verify + ARP round trip
    serial::write_str("[TEST] Storage self-test...\n");
    virtio::blk::self_test();
    serial::write_str("[TEST] Network ARP test...\n");
    virtio::net::arp_demo();

    // Mount the exFAT filesystem and exercise read + create paths
    serial::write_str("[TEST] exFAT filesystem...\n");
    fs::exfat::demo();

    // Initialize IPC (stub)
    serial::write_str("[INIT] IPC subsystem...\n");
    ipc::init();

    // Test heap allocation
    serial::write_str("[TEST] Heap allocation test...\n");
    {
        use alloc::vec::Vec;
        use alloc::boxed::Box;
        use alloc::format;

        // Box test
        let boxed_val = Box::new(42u64);
        serial::write_str("[TEST] Box: ");
        serial::write_dec(*boxed_val);
        serial::write_str("\n");

        // Vec test
        let mut numbers = Vec::new();
        for i in 0..10 {
            numbers.push(i * 100);
        }
        serial::write_str("[TEST] Vec: [");
        for (i, n) in numbers.iter().enumerate() {
            if i > 0 { serial::write_str(", "); }
            serial::write_dec(*n as u64);
        }
        serial::write_str("]\n");

        // Format test
        let msg = format!("test-format-{}", 42);
        serial::write_str("[TEST] String: ");
        serial::write_str(&msg);
        serial::write_str("\n");

        serial::write_str("[TEST] Heap allocation OK!\n");
    }

    // Create demo tasks — idle CPUs are kicked with a reschedule IPI and
    // start running these immediately, even before the BSP enables
    // interrupts for itself
    serial::write_str("[SCHED] Creating demo worker tasks...\n");
    for _ in 0..6 {
        scheduler::create_task(worker_task, scheduler::PRIORITY_DEFAULT, "worker");
    }
    // Keyboard echo task — consumes the keyboard driver's char queue
    scheduler::create_task(kbd_echo_task, scheduler::PRIORITY_DEFAULT, "kbd_echo");

    // Print before sti: once the BSP joins the rotation, this boot context
    // is the BSP's idle task and only runs when the BSP has nothing to do
    serial::write_str("\n✓ MMURTL/RS kernel ready — ");
    serial::write_dec(smp::cpus_online() as u64);
    serial::write_str(" CPU(s) scheduling.\n");

    // Enable interrupts — the BSP joins the scheduling rotation
    x86_64::instructions::interrupts::enable();

    // Idle loop
    loop {
        x86_64::instructions::hlt();
    }
}

/// Demo worker task — prints its task ID, the CPU it is currently running
/// on, and a counter. With multiple CPUs scheduling, the same task shows
/// up on different CPUs over time as it migrates.
extern "C" fn worker_task() -> ! {
    use core::fmt::Write;

    let tid = scheduler::current_task_id();
    let mut count = 0u64;
    loop {
        // Build the line first, emit with a single write_str so lines from
        // workers on different CPUs don't interleave mid-line
        let cpu = scheduler::current_cpu();
        let mut line: heapless::String<80> = heapless::String::new();
        let _ = write!(line, "[T{} on CPU{}] count={}\n", tid, cpu, count);
        serial::write_str(&line);
        count += 1;

        // Busy-wait to eat up our time slice
        for _ in 0..5000000 {
            core::hint::spin_loop();
        }
    }
}

/// Keyboard echo task — prints characters typed on the PS/2 keyboard,
/// demonstrating IRQ → driver queue → task consumption across CPUs.
extern "C" fn kbd_echo_task() -> ! {
    use core::fmt::Write;

    loop {
        while let Some(c) = keyboard::pop_char() {
            let cpu = scheduler::current_cpu();
            let mut line: heapless::String<48> = heapless::String::new();
            if c.is_ascii_graphic() || c == b' ' {
                let _ = write!(line, "[KBD on CPU{}] '{}'\n", cpu, c as char);
            } else {
                let _ = write!(line, "[KBD on CPU{}] 0x{:02x}\n", cpu, c);
            }
            serial::write_str(&line);
        }
        // Nothing pending — let the time slice go by
        for _ in 0..100000 {
            core::hint::spin_loop();
        }
    }
}

/// Bootloader config that enables physical memory offset mapping
use bootloader_api::config::{BootloaderConfig, Mappings, Mapping};

const BOOTLOADER_CONFIG: BootloaderConfig = {
    let mut config = BootloaderConfig::new_default();
    config.mappings.physical_memory = Option::Some(Mapping::Dynamic);
    config
};

// Define entry point using bootloader_api macro with custom config
bootloader_api::entry_point!(kernel_main, config = &BOOTLOADER_CONFIG);

/// Panic handler
#[panic_handler]
fn panic(info: &PanicInfo) -> ! {
    serial::write_str("\n\n!!! KERNEL PANIC !!!\n");
    if let Some(msg) = info.message().as_str() {
        serial::write_str("Message: ");
        serial::write_str(msg);
        serial::write_str("\n");
    }
    if let Some(loc) = info.location() {
        serial::write_str("At: ");
        serial::write_str(loc.file());
        serial::write_str(":");
        serial::write_dec(loc.line() as u64);
        serial::write_str("\n");
    }
    loop {
        x86_64::instructions::hlt();
    }
}
