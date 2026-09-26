//! arm64 multi-core boot: PSCI or spin tables, per the device tree.
//!
//! PSCI (QEMU `virt`, most servers): secondary CPUs are powered off;
//! `CPU_ON` (through HVC or SMC) starts one at `secondary_start` with the
//! MMU off and our context pointer in x0.
//!
//! Spin table (Raspberry Pi, `enable-method = "spin-table"`): the firmware
//! parks each secondary in a `wfe` loop polling its `cpu-release-addr`.
//! We write `secondary_spin_entry`'s address there, clean it to the point
//! of coherency (the parked CPU's caches are off), and `sev`. The entry
//! gets no argument, so the context pointer is passed in `AP_SPIN_BOOT`.
//!
//! Either way the stub loads the boot CPU's MMU
//! configuration and jumps to `arm64_secondary_main`, which installs the
//! vectors, enables the CPU's GIC interface, and joins the scheduler —
//! exactly the role `ap_entry` plays on amd64.

use alloc::boxed::Box;
use alloc::vec;
use core::arch::asm;
use core::sync::atomic::{AtomicBool, AtomicU64, AtomicUsize, Ordering};

use super::fdt::{MachineInfo, PsciConduit};

const PSCI_CPU_ON: u64 = 0xC400_0003;
const AP_STACK_SIZE: usize = 64 * 1024;

static CPUS_ONLINE: AtomicUsize = AtomicUsize::new(1);
static AP_READY: AtomicBool = AtomicBool::new(false);

pub fn cpus_online() -> usize {
    CPUS_ONLINE.load(Ordering::SeqCst)
}

/// Read by `secondary_start` with the MMU off (field order is load-bearing)
#[repr(C)]
struct ApBoot {
    stack_top: u64,
    cpu: u64,
    mair: u64,
    tcr: u64,
    ttbr0: u64,
    sctlr: u64,
}

extern "C" {
    fn secondary_start();
    fn secondary_spin_entry();
}

/// Context for the CPU being released from a spin table (read by
/// `secondary_spin_entry` with the MMU off)
#[no_mangle]
static AP_SPIN_BOOT: AtomicU64 = AtomicU64::new(0);

fn psci_call(conduit: PsciConduit, func: u64, a1: u64, a2: u64, a3: u64) -> i64 {
    let mut r = func;
    unsafe {
        match conduit {
            PsciConduit::Hvc => asm!("hvc #0", inout("x0") r, in("x1") a1, in("x2") a2, in("x3") a3),
            PsciConduit::Smc => asm!("smc #0", inout("x0") r, in("x1") a1, in("x2") a2, in("x3") a3),
            PsciConduit::None => return -1,
        }
    }
    r as i64
}

/// Clean a structure to the point of coherency so a CPU with its caches
/// off reads what we wrote
fn clean_dcache(addr: u64, len: usize) {
    let ctr: u64;
    unsafe { asm!("mrs {}, ctr_el0", out(reg) ctr) };
    let step = 4u64 << ((ctr >> 16) & 0xF); // smallest D-cache line
    let mut line = addr & !(step - 1);
    while line < addr + len as u64 {
        unsafe { asm!("dc civac, {}", in(reg) line) };
        line += step;
    }
    unsafe { asm!("dsb sy") };
}

fn mpidr() -> u64 {
    let v: u64;
    unsafe { asm!("mrs {}, mpidr_el1", out(reg) v) };
    v & 0xFF_00FF_FFFF
}

/// Start every other CPU in the device tree, one at a time
pub fn boot_secondaries(info: &MachineInfo) {
    let spin = info.cpu_release[..info.cpu_count].iter().any(|&r| r != 0);
    if info.psci == PsciConduit::None && !spin {
        crate::serial::write_line("[SMP] No PSCI or spin table — running on the boot CPU only");
        return;
    }
    let me = mpidr();
    let regs = super::mmu::regs();
    let mut next_cpu = 1u64;

    for (i, &target) in info.cpus[..info.cpu_count].iter().enumerate() {
        if target == me {
            continue;
        }
        let stack: &'static mut [u8] = Box::leak(vec![0u8; AP_STACK_SIZE].into_boxed_slice());
        let boot = Box::leak(Box::new(ApBoot {
            stack_top: stack.as_ptr() as u64 + AP_STACK_SIZE as u64,
            cpu: next_cpu,
            mair: regs.mair,
            tcr: regs.tcr,
            ttbr0: regs.ttbr0,
            sctlr: regs.sctlr,
        }));
        clean_dcache(boot as *const ApBoot as u64, core::mem::size_of::<ApBoot>());

        AP_READY.store(false, Ordering::SeqCst);
        let release = info.cpu_release[i];
        if release != 0 {
            // Spin table: hand over the context, then the entry point
            AP_SPIN_BOOT.store(boot as *const ApBoot as u64, Ordering::SeqCst);
            clean_dcache(AP_SPIN_BOOT.as_ptr() as u64, 8);
            unsafe {
                core::ptr::write_volatile(release as *mut u64, secondary_spin_entry as *const () as u64);
            }
            clean_dcache(release, 8);
            unsafe { asm!("sev") };
        } else {
            let r = psci_call(
                info.psci,
                PSCI_CPU_ON,
                target,
                secondary_start as *const () as u64,
                boot as *const ApBoot as u64,
            );
            if r != 0 {
                crate::serial::write_str("[SMP] PSCI CPU_ON failed for MPIDR 0x");
                crate::serial::write_hex(target);
                crate::serial::write_str("\n");
                continue;
            }
        }
        // Wait (up to ~1 s) for it to join the scheduler
        let mut ok = false;
        for _ in 0..1000 {
            if AP_READY.load(Ordering::SeqCst) {
                ok = true;
                break;
            }
            super::timer::delay_ms(1);
        }
        if ok {
            next_cpu += 1;
        } else {
            crate::serial::write_line("[SMP] CPU did not come online");
        }
    }
    crate::serial::write_str("[SMP] ");
    crate::serial::write_dec(cpus_online() as u64);
    crate::serial::write_str(" CPU(s) online\n");
}

/// First Rust code on a secondary CPU (MMU on, SP = its boot stack)
#[no_mangle]
extern "C" fn arm64_secondary_main(cpu: u64) -> ! {
    super::exceptions::init();
    super::irq::init_cpu();
    crate::scheduler::register_ap(cpu as usize);
    CPUS_ONLINE.fetch_add(1, Ordering::SeqCst);

    use core::fmt::Write;
    let mut line: heapless::String<64> = heapless::String::new();
    let _ = write!(line, "[SMP] CPU {} online (MPIDR 0x{:x}), scheduling\n", cpu, mpidr());
    crate::serial::write_str(&line);

    AP_READY.store(true, Ordering::SeqCst);

    // Idle loop — the scheduler switches away from here whenever there is
    // work, and back whenever there isn't
    super::enable_interrupts();
    loop {
        super::halt();
    }
}
