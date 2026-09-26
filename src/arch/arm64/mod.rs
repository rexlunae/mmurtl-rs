//! arm64 (AArch64) port — QEMU `virt` and similar device-tree machines.
//!
//! Boot from an ELF at EL1 (dropping from EL2 if needed); PL011 serial;
//! the device tree for RAM, CPUs, and devices; an identity-mapped MMU
//! with EL0/EL1 permission bits; an EL1 vector table; GICv2/GICv3 + the generic
//! timer; PSCI multi-core boot; virtio-mmio devices; EL0 userspace via
//! `svc #0`.

pub mod boot;
pub mod exceptions;
pub mod fdt;
pub mod gic;
pub mod memory;
pub mod mmu;
pub mod serial;
pub mod smp;
pub mod timer;
pub mod user_programs;
pub mod virtio_mmio;

use core::arch::asm;

pub use exceptions::{kernel_context, user_context, TaskContext};
pub use memory::{heap_extend, heap_init, phys_to_virt, user_access_begin, user_access_end};
pub use mmu::{free_address_space, map_user_page, new_address_space, query_page, switch_address_space};

/// Architecture name for the boot log
pub const NAME: &str = "arm64";

/// ELF e_machine for user programs (EM_AARCH64)
pub const ELF_MACHINE: u16 = 0xB7;

/// How user code enters the kernel
pub const SYSCALL_MECHANISM: &str = "svc #0 from EL0";

// ========================================================================
// Interrupts
// ========================================================================

/// Run `f` with IRQs masked on this CPU (restoring the previous mask)
pub fn without_interrupts<R>(f: impl FnOnce() -> R) -> R {
    let daif: u64;
    unsafe {
        asm!("mrs {}, daif", out(reg) daif);
        asm!("msr daifset, #2");
    }
    let r = f();
    if daif & (1 << 7) == 0 {
        unsafe { asm!("msr daifclr, #2") };
    }
    r
}

/// Unmask IRQs on this CPU
pub fn enable_interrupts() {
    unsafe { asm!("msr daifclr, #2") };
}

/// Sleep until the next interrupt
pub fn halt() {
    unsafe { asm!("wfi") };
}

// ========================================================================
// CPU identity + IPIs
// ========================================================================

/// Scheduler index of the calling CPU (kept in TPIDR_EL1)
pub fn cpu_index() -> usize {
    let v: u64;
    unsafe { asm!("mrs {}, tpidr_el1", out(reg) v) };
    v as usize
}

/// Record the calling CPU's scheduler index
pub fn set_cpu_index(cpu: usize) {
    unsafe { asm!("msr tpidr_el1, {}", in(reg) cpu as u64) };
}

/// The calling CPU's IPI target (GICv2 interface number or GICv3
/// affinity)
pub fn hw_cpu_id() -> u32 {
    gic::cpu_target_id()
}

pub fn ipi_available() -> bool {
    true
}

/// Kick the CPU identified by `hw_id` into its scheduler
pub fn send_resched_ipi(hw_id: u32) {
    gic::send_resched(hw_id);
}

pub fn cpus_online() -> usize {
    smp::cpus_online()
}

// ========================================================================
// Time + task switching
// ========================================================================

/// Start the calling CPU's scheduler tick (the EL1 virtual timer)
pub fn start_tick(hz: u32, _boot_cpu: bool) {
    timer::start(hz);
}

pub fn delay_ms(ms: u32) {
    timer::delay_ms(ms);
}

/// Give up the CPU: `svc` from EL1 enters the vector table's yield path
pub fn yield_now() {
    unsafe { asm!("svc #0") };
}

/// Nothing to do: a user task's exceptions land on its kernel stack
/// because SP_EL1 is left at its top when we ERET into EL0
pub fn on_switch_to_user(_cpu: usize, _kstack_top: u64) {}
