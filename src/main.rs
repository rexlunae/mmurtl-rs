//! MMURTL/RS — Message-passing multitasking real-time kernel in Rust
//!
//! A Rust port inspired by Richard Burgess's MMURTL kernel (1994).
//! Architectures: amd64 (x86_64 long mode, BIOS/UEFI via `bootloader`) and
//! arm64 (AArch64 EL1, QEMU `virt`). Everything outside `arch/` —
//! scheduler, RQB IPC, memory management, virtio drivers, exFAT, syscalls,
//! userspace — is shared by both ports.

#![no_std]
#![no_main]
#![cfg_attr(target_arch = "x86_64", feature(abi_x86_interrupt))]

extern crate alloc;

mod arch;
mod serial;
mod memory;
mod scheduler;
mod ipc;
mod pci;
mod virtio;
mod keyboard;
mod fs;
mod syscall;
mod userspace;
mod shell;
mod security;
#[cfg(target_arch = "x86_64")]
mod microcode;

use core::panic::PanicInfo;

/// MMURTL/RS version info
pub const VERSION: &str = env!("CARGO_PKG_VERSION");
pub const KERNEL_NAME: &str = "MMURTL/RS";
pub const BOOT_BANNER: &str = include_str!("banner.txt");

/// Print the boot banner (right after the console comes up)
pub fn print_banner() {
    serial::write_str(KERNEL_NAME);
    serial::write_str(" v");
    serial::write_str(VERSION);
    serial::write_str(" (");
    serial::write_str(arch::NAME);
    serial::write_str(") booting...\n\n");
    serial::write_str(BOOT_BANNER);
    serial::write_str("\n");
}

/// Architecture-neutral second half of boot. The architecture has brought
/// up the console, memory, interrupts, the scheduler, all CPUs, and the
/// devices it found; this runs the self-tests and starts the tasks.
pub fn kernel_run() -> ! {
    // Driver proof-of-life: block device write/read/verify + ARP round trip
    serial::write_str("[TEST] Storage self-test...\n");
    virtio::blk::self_test();
    serial::write_str("[TEST] Network ARP test...\n");
    virtio::net::arp_demo();

    // Discover a FAT volume or mount and exercise the exFAT filesystem
    serial::write_str("[INIT] Filesystem discovery...\n");
    fs::init();

    // Initialize IPC (blocking RQB message passing)
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

    #[cfg(target_arch = "aarch64")]
    { arch::mmu::audit_kernel_wx(); arch::wx::self_test(); }
    memory::user::uaccess_self_test();
    #[cfg(target_arch = "x86_64")]
    { arch::wx::audit_kernel_wx(); arch::protection::self_test(); }
    #[cfg(target_arch = "x86_64")]
    arch::interrupts::nospec_interrupt_self_test();

    // Create demo tasks — idle CPUs are kicked with a reschedule IPI and
    // start running these immediately, even before the BSP enables
    // interrupts for itself
    serial::write_str("[SCHED] Creating demo worker tasks...\n");
    for _ in 0..4 {
        scheduler::create_task(worker_task, scheduler::PRIORITY_DEFAULT, "worker");
    }
    // RQB IPC demo: a text service, concurrent clients, error-path checks
    serial::write_str("[IPC] Starting IPC demo tasks...\n");
    ipc::demo();

    // Reclaims exited tasks' kernel stacks and address spaces
    scheduler::start_reaper();

    // Userspace: user-mode programs talking to the kernel via syscalls
    syscall::init();
    serial::write_str("[USER] Loading user-mode programs...\n");
    userspace::demo();
    // The CLI owns console input after the boot checks finish.
    shell::start();

    // Print before sti: once the BSP joins the rotation, this boot context
    // is the BSP's idle task and only runs when the BSP has nothing to do
    serial::write_str("\n✓ MMURTL/RS kernel ready — ");
    serial::write_dec(arch::cpus_online() as u64);
    serial::write_str(" CPU(s) scheduling.\n");

    // Enable interrupts — the boot CPU joins the scheduling rotation
    arch::enable_interrupts();

    // Idle loop
    loop {
        arch::halt();
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
        if shell::active() {
            scheduler::exit_current();
        }
        // Build the line first, emit with a single write_str so lines from
        // workers on different CPUs don't interleave mid-line
        let cpu = scheduler::current_cpu();
        let mut line: heapless::String<80> = heapless::String::new();
        let _ = write!(line, "[T{} on CPU{}] count={}\n", tid, cpu, count);
        serial::write_str(&line);
        count += 1;

        // Burn part of a time slice (so preemption + migration still
        // show), then block — sleeping tasks cost no CPU
        for _ in 0..2000000 {
            core::hint::spin_loop();
        }
        scheduler::sleep_ms(1000);
    }
}

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
        arch::halt();
    }
}
