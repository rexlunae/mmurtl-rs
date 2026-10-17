//! Boot-time permission finalization and synchronous TLB invalidation.
use core::sync::atomic::{AtomicBool, Ordering};
const VECTOR: u8 = 0x32;
static ACK: [AtomicBool; crate::scheduler::MAX_CPUS] = [const { AtomicBool::new(false) }; crate::scheduler::MAX_CPUS];
static SHOOTDOWN: spin::Mutex<()> = spin::Mutex::new(());

/// Runtime shared-kernel invalidation. The caller has released FRAME_LOCK
/// and the scheduler lock, and keeps reclaimed frames private until we return.
pub fn invalidate_shared() {
    let targets = crate::scheduler::boot_cpu_targets();
    let _guard = SHOOTDOWN.lock();
    let current = super::cpu_index();
    unsafe { flush_all(); }
    for (cpu, target) in targets.iter().enumerate() {
        if cpu == current { continue; }
        if let Some(id) = target {
            ACK[cpu].store(false, Ordering::Release);
            super::apic::send_ipi(*id, VECTOR);
        }
    }
    for (cpu, target) in targets.iter().enumerate() {
        if cpu == current || target.is_none() { continue; }
        for _ in 0..1000 {
            if ACK[cpu].load(Ordering::Acquire) { break; }
            super::apic::pit_wait_ms(1);
        }
        assert!(ACK[cpu].load(Ordering::Acquire), "runtime stack TLB shootdown timeout");
    }
}
unsafe fn flush_all() {
    // CR3 alone does not invalidate global translations.
    let cr4: u64;
    core::arch::asm!("mov {}, cr4", out(reg) cr4, options(nostack));
    if cr4 & (1 << 7) != 0 {
        core::arch::asm!("mov cr4, {}", "mov cr4, {}", in(reg) cr4 & !(1 << 7), in(reg) cr4, options(nostack));
    } else {
        let _cr3: u64;
        core::arch::asm!("mov {0}, cr3", "mov cr3, {0}", out(reg) _cr3, options(nostack));
    }
}

pub fn tlb_handler() {
    super::protection::clear_access();
    unsafe { flush_all(); }
    super::apic::eoi();
    ACK[super::cpu_index()].store(true, Ordering::Release);
}

/// BSP-only, once, after admission is sealed and before user tasks exist.
pub fn finalize_trampoline(installed: bool, possible_late_ap: bool) {
    assert_eq!(super::cpu_index(), 0);
    if !installed { return; }
    let targets = crate::scheduler::boot_cpu_targets();
    crate::memory::heap::with_frame_allocator(|fa| unsafe {
        let mut adapter = super::page_table::BumpFrameAllocator::new(fa);
        let alias = super::page_table::phys_to_virt(x86_64::PhysAddr::new(super::smp::TRAMPOLINE_PHYS));
        super::page_table::set_page_readonly(alias, &mut adapter).expect("trampoline alias protection");
        if !possible_late_ap {
            super::page_table::unmap_page(x86_64::structures::paging::Page::containing_address(x86_64::VirtAddr::new(super::smp::TRAMPOLINE_PHYS))).expect("trampoline unmap");
        }
    }).expect("frame allocator");
    unsafe { flush_all(); }
    for (cpu, hw_id) in targets.iter().enumerate().skip(1) {
        if let Some(id) = hw_id {
            ACK[cpu].store(false, Ordering::Release);
            super::apic::send_ipi(*id, VECTOR);
        }
    }
    for (cpu, target) in targets.iter().enumerate().skip(1) {
        if target.is_none() { continue; }
        for _ in 0..1000 {
            if ACK[cpu].load(Ordering::Acquire) { break; }
            super::apic::pit_wait_ms(1);
        }
        assert!(ACK[cpu].load(Ordering::Acquire), "boot TLB shootdown timeout");
    }
    crate::serial::write_line(if possible_late_ap {
        "[MMU] Late AP possible: trampoline retained RX, physical alias RO/NX"
    } else { "[MMU] AP trampoline unmapped; physical alias RO/NX" });
}

pub fn audit_kernel_wx() {
    use x86_64::structures::paging::{PageTable, page_table::PageTableFlags as F};
    unsafe fn visit(t: *const PageTable, level: usize, writable: bool, user: bool, nx: bool) {
        for e in (&*t).iter() {
            let f = e.flags();
            if !f.contains(F::PRESENT) { continue; }
            let w = writable && f.contains(F::WRITABLE);
            let u = user && f.contains(F::USER_ACCESSIBLE);
            let n = nx || f.contains(F::NO_EXECUTE);
            if level == 3 || (level > 0 && f.contains(F::HUGE_PAGE)) {
                assert!(!u, "EL0 access to kernel mapping");
                assert!(!w || n, "writable executable kernel mapping");
            } else { visit(super::page_table::phys_to_virt(e.addr()).as_ptr(), level + 1, w, u, n); }
        }
    }
    unsafe { visit(super::page_table::active_pml4(), 0, true, true, false); }
    crate::serial::write_line("[MMU] Kernel W^X audit passed");
}
