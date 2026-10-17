//! Enable only access-protection features shared by every online CPU.
use core::sync::atomic::{AtomicBool, AtomicU8, AtomicU64, Ordering};
const SMEP: u64 = 1 << 20;
const SMAP: u64 = 1 << 21;
const UMIP: u64 = 1 << 11;
static CAPS: [AtomicU64; 64] = [const { AtomicU64::new(0) }; 64];
static APPLIED: [AtomicBool; 64] = [const { AtomicBool::new(false) }; 64];
static READY: AtomicBool = AtomicBool::new(false);
static COMMON: AtomicU64 = AtomicU64::new(0);
/// Assembly entries read this byte without clobbering any incoming register.
/// Published only after every online CPU's support has been intersected.
#[no_mangle]
pub static SMAP_ENTRY_ENABLED: AtomicU8 = AtomicU8::new(0);

pub fn enumerate(cpu: usize) {
    let mut caps = 0;
    unsafe {
        if core::arch::x86_64::__cpuid(0).eax >= 7 {
            let leaf = core::arch::x86_64::__cpuid_count(7, 0);
            if leaf.ebx & (1 << 7) != 0 { caps |= SMEP; }
            if leaf.ebx & (1 << 20) != 0 { caps |= SMAP; }
            if leaf.ecx & (1 << 2) != 0 { caps |= UMIP; }
        }
    }
    unsafe {
        let mut cr4: u64;
        core::arch::asm!("mov {}, cr4", out(reg) cr4, options(nomem, nostack));
        cr4 &= !(SMEP | SMAP | UMIP);
        core::arch::asm!("mov cr4, {}", in(reg) cr4, options(nostack));
        core::arch::asm!("pushfq", "and qword ptr [rsp], -262145", "popfq"); // clear inherited AC
    }
    CAPS[cpu].store(caps, Ordering::Release);
}

pub fn finalize() {
    let mut common = SMEP | SMAP | UMIP;
    for cpu in 0..super::smp::cpus_online() { common &= CAPS[cpu].load(Ordering::Acquire); }
    COMMON.store(common, Ordering::Release);
    SMAP_ENTRY_ENABLED.store(u8::from(common & SMAP != 0), Ordering::Release);
    READY.store(true, Ordering::Release);
    apply_current(0);
    crate::serial::write_str("[CPU] common access protections:");
    for (mask, name) in [(SMEP, " SMEP"), (SMAP, " SMAP"), (UMIP, " UMIP")] {
        if common & mask != 0 { crate::serial::write_str(name); }
    }
    crate::serial::write_line(" (enforced before each CPU runs tasks)");
    // This kernel's code and physmap may occupy the lower canonical half.
    // LASS would reject legitimate supervisor instruction/data accesses.
    crate::serial::write_line("[CPU] LASS deferred: kernel mappings require higher-half relocation");
}

/// Called with interrupts disabled before choosing a task's page tables.
/// APs may have parked before finalize; they apply the common policy on
/// their first scheduler entry, before any task can enter ring 3.
pub fn apply_current(cpu: usize) {
    let common = COMMON.load(Ordering::Acquire);
    // Until finalize, don't mark this CPU configured: an early idle tick
    // must not prevent the final policy from being applied later.
    if !READY.load(Ordering::Acquire) || APPLIED[cpu].load(Ordering::Relaxed) { return; }
    unsafe {
        let (mut cr0, mut cr4): (u64, u64);
        core::arch::asm!("mov {}, cr0", out(reg) cr0, options(nomem, nostack));
        core::arch::asm!("mov {}, cr4", out(reg) cr4, options(nomem, nostack));
        cr0 |= 1 << 16; // WP: supervisor writes obey read-only mappings
        cr4 |= common;
        core::arch::asm!("mov cr0, {}", in(reg) cr0, options(nostack));
        core::arch::asm!("mov cr4, {}", in(reg) cr4, options(nostack));
        clear_access();
        core::arch::asm!("mov {}, cr0", out(reg) cr0, options(nomem, nostack));
        core::arch::asm!("mov {}, cr4", out(reg) cr4, options(nomem, nostack));
        assert!(cr0 & (1 << 16) != 0 && cr4 & common == common);
    }
    APPLIED[cpu].store(true, Ordering::Release);
}

pub fn smap_enabled() -> bool { SMAP_ENTRY_ENABLED.load(Ordering::Relaxed) != 0 }
#[inline(always)]
pub fn clear_access() {
    if smap_enabled() { unsafe { core::arch::asm!("clac", options(nostack)); } }
}
pub fn umip_enabled() -> bool { COMMON.load(Ordering::Acquire) & UMIP != 0 }
