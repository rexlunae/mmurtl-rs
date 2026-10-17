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
    super::fpu::init();
    super::set_user_counter_access(false);
    serial::init();
    serial::write_line("[FPU] user FP/SIMD disabled on every CPU (no context switching yet)");
    let mut args = [0u8; 512];
    let args_len = super::cpu_features::bootargs(&mut args);
    let args = args_len.and_then(|len| core::str::from_utf8(&args[..len]).ok())
        .unwrap_or(option_env!("MMURTL_BOOTARGS").unwrap_or(""));
    crate::security::init(args.trim_end_matches('\0'));
    crate::print_banner();
    crate::serial::write_line("[TIME] user high-resolution counters: per-task (default off)");

    // Log boot info — memory regions
    let region_count = boot_info.memory_regions.len();
    serial::write_str("[INFO] Memory regions: ");
    serial::write_dec(region_count as u64);
    serial::write_str("\n");

    // Log physical memory offset
    match boot_info.physical_memory_offset {
        Optional::Some(offset) => {
            serial::write_str("[INFO] Physical memory offset: ");
            crate::kptr::write(offset);
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
    super::memory_encryption::report();

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
    let module = match boot_info.ramdisk_addr { Optional::Some(a) => Some(a), Optional::None => None };
    super::microcode::init(module, boot_info.ramdisk_len);
    assert!(super::microcode::apply(0, || {}));
    crate::random::init();
    if !crate::random::ready() { crate::serial::write_line("[KASLR] WARNING: kernel heap entropy below 128 bits; placement is not cryptographically assured"); }
    crate::memory::heap::init_heap();
    crate::kptr::init();
    crate::kptr::report(kernel_main as *const () as u64, super::page_table::physical_memory_offset());
    super::protection::enumerate(0);
    super::memory::protect_image_aliases(boot_info.kernel_addr, boot_info.kernel_len, boot_info.kernel_image_offset);
    super::gdt::protect_bsp_stacks();
    crate::scheduler::debug_lock_self_test();

    // Parse ACPI tables (MADT: CPUs, Local APIC, I/O APIC)
    serial::write_str("[INIT] ACPI...\n");
    super::acpi::init(rsdp_addr);

    // Switch from PIC/PIT to Local APIC + I/O APIC
    serial::write_str("[INIT] APIC...\n");
    if !super::apic::init() { super::interrupts::unmask_console(); }
    super::serial::enable_rx_irq();
    crate::topology::bsp_ready();

    // Initialize the scheduler on the BSP (must precede AP boot: APs
    // register themselves and start their timers as they come up)
    serial::write_str("[INIT] Scheduler...\n");
    crate::scheduler::init();

    // Boot the application processors — each joins the scheduler
    serial::write_str("[INIT] SMP...\n");
    super::smp::boot_aps();
    super::microcode::seal();
    super::protection::finalize();
    super::smp::finalize_trampoline();
    crate::security::finalize();
    crate::topology::report();
    super::store_bypass::init();
    crate::security::report();

    // Initialize PCI and USB
    serial::write_str("[INIT] PCI bus...\n");
    let devices = crate::pci::scan();

    serial::write_str("[INIT] USB subsystem...\n");
    super::usb::init();

    // Virtio drivers: storage (virtio-blk) + network (virtio-net)
    serial::write_str("[INIT] Virtio drivers...\n");
    crate::virtio::pci::init(&devices);

    // Architecture-neutral second half: self-tests, IPC, tasks, userspace
    crate::kernel_run()
}

/// Bootloader config that enables physical memory offset mapping
use bootloader_api::config::{BootloaderConfig, Mapping};

const BOOTLOADER_CONFIG: BootloaderConfig = {
    let mut config = BootloaderConfig::new_default();
    config.mappings.aslr = true;
    // Reserve the upper region for independently randomized heap mappings.
    config.mappings.dynamic_range_start = Some(crate::security::kaslr::AMD_DYNAMIC_START);
    config.mappings.dynamic_range_end = Some(crate::security::kaslr::AMD_DYNAMIC_END);
    config.mappings.physical_memory = Option::Some(Mapping::Dynamic);
    config
};

// Define entry point using bootloader_api macro with custom config
bootloader_api::entry_point!(kernel_main, config = &BOOTLOADER_CONFIG);

