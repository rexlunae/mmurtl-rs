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

    // Initialize memory management
    serial::write_str("[INIT] Memory manager...\n");
    memory::init(boot_info);

    // Initialize PCI and USB
    serial::write_str("[INIT] PCI bus...\n");
    let _devices = pci::scan();

    serial::write_str("[INIT] USB subsystem...\n");
    usb::init();

    // Initialize scheduler (PIT-based preemptive multitasking)
    serial::write_str("[INIT] Scheduler...\n");
    scheduler::init();

    // Initialize IPC (stub)
    serial::write_str("[INIT] IPC subsystem...\n");
    ipc::init();

    // Create demo tasks (these run concurrently once interrupts are enabled)
    serial::write_str("[SCHED] Creating demo tasks...\n");
    scheduler::create_task(task_a, scheduler::PRIORITY_DEFAULT, "task_a");
    scheduler::create_task(task_b, scheduler::PRIORITY_DEFAULT, "task_b");

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

    // Enable interrupts
    serial::write_str("[INFO] Enabling interrupts...\n");
    x86_64::instructions::interrupts::enable();

    serial::write_str("\n✓ MMURTL/RS kernel ready.\n");

    // Idle loop
    loop {
        x86_64::instructions::hlt();
    }
}

/// Demo task A — prints a counter on each time slice
/// Runs concurrently with task_b via preemptive multitasking
extern "C" fn task_a() -> ! {
    let mut count = 0u64;
    loop {
        crate::serial::write_str("[A] Hello from task_a! count=");
        crate::serial::write_dec(count);
        crate::serial::write_str("\n");
        count += 1;

        // Busy-wait to eat up our time slice
        for _ in 0..5000000 {
            core::hint::spin_loop();
        }
    }
}

/// Demo task B — prints a counter on each time slice
extern "C" fn task_b() -> ! {
    let mut count = 0u64;
    loop {
        crate::serial::write_str("[B] Hello from task_b! count=");
        crate::serial::write_dec(count);
        crate::serial::write_str("\n");
        count += 1;

        // Busy-wait to eat up our time slice
        for _ in 0..5000000 {
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
