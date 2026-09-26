//! amd64 boot: the `bootloader` crate enters `kernel_main` in long mode
//! with a physical-memory offset mapping. This brings up the x86-specific
//! machinery (GDT/IDT, PIC→APIC, ACPI, SMP, PCI, virtio-pci), then hands
//! over to the architecture-neutral `crate::kernel_run`.

use bootloader_api::info::Optional;
use bootloader_api::BootInfo;

/// Kernel entry point — called by bootloader
fn kernel_main(boot_info: &'static mut BootInfo) -> ! {
    use crate::serial;
    // Initialize serial early for debugging, then the banner
    serial::init();
    crate::print_banner();

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
    super::gdt::init();

    serial::write_str("[INIT] IDT...\n");
    super::interrupts::init();

    // Initialize PIC (Programmable Interrupt Controller)
    serial::write_str("[INIT] PIC...\n");
    super::interrupts::init_pic();

    // Save the RSDP address before memory takes ownership of boot_info
    let rsdp_addr = match boot_info.rsdp_addr {
        Optional::Some(addr) => Some(addr),
        Optional::None => None,
    };

    // Initialize memory management
    serial::write_str("[INIT] Memory manager...\n");
    let phys_offset = match boot_info.physical_memory_offset {
        Optional::Some(offset) => Some(offset),
        Optional::None => None,
    };
    super::memory::init(&boot_info.memory_regions, phys_offset);

    // Parse ACPI tables (MADT: CPUs, Local APIC, I/O APIC)
    serial::write_str("[INIT] ACPI...\n");
    super::acpi::init(rsdp_addr);

    // Switch from PIC/PIT to Local APIC + I/O APIC
    serial::write_str("[INIT] APIC...\n");
    super::apic::init();

    // Initialize the scheduler on the BSP (must precede AP boot: APs
    // register themselves and start their timers as they come up)
    serial::write_str("[INIT] Scheduler...\n");
    crate::scheduler::init();

    // Boot the application processors — each joins the scheduler
    serial::write_str("[INIT] SMP...\n");
    super::smp::boot_aps();

    // Initialize PCI and USB
    serial::write_str("[INIT] PCI bus...\n");
    let devices = super::pci::scan();

    serial::write_str("[INIT] USB subsystem...\n");
    super::usb::init();

    // Virtio drivers: storage (virtio-blk) + network (virtio-net)
    serial::write_str("[INIT] Virtio drivers...\n");
    super::virtio_pci::init(&devices);

    // Architecture-neutral second half: self-tests, IPC, tasks, userspace
    crate::kernel_run()
}

/// Bootloader config that enables physical memory offset mapping
use bootloader_api::config::{BootloaderConfig, Mapping};

const BOOTLOADER_CONFIG: BootloaderConfig = {
    let mut config = BootloaderConfig::new_default();
    config.mappings.physical_memory = Option::Some(Mapping::Dynamic);
    config
};

// Define entry point using bootloader_api macro with custom config
bootloader_api::entry_point!(kernel_main, config = &BOOTLOADER_CONFIG);

