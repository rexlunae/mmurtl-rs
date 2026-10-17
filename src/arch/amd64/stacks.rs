use super::page_table;
use x86_64::{VirtAddr, PhysAddr, structures::paging::{Page, PhysFrame, page_table::PageTableFlags as F}};
pub fn map(va: u64, pa: u64, fa: &mut crate::memory::frame_allocator::FrameAllocator) {
    let mut adapter = page_table::BumpFrameAllocator::new(fa);
    unsafe { page_table::map_page(Page::containing_address(VirtAddr::new(va)),
        PhysFrame::containing_address(PhysAddr::new(pa)), F::WRITABLE | F::NO_EXECUTE,
        &mut adapter).expect("guarded stack map"); }
}
pub fn mapped(va: u64) -> bool { page_table::query_page(VirtAddr::new(va)).is_some() }
pub fn unmap(va: u64) -> u64 {
    unsafe { page_table::unmap_page(Page::containing_address(VirtAddr::new(va))).expect("stack unmap").start_address().as_u64() }
}
pub fn invalidate() { super::wx::invalidate_shared(); }
pub fn interrupts_enabled() -> bool { x86_64::instructions::interrupts::are_enabled() }
pub fn on_emergency_stack() -> bool {
    let sp: u64; unsafe { core::arch::asm!("mov {}, rsp",out(reg) sp,options(nomem,nostack)); }
    let top = super::gdt::double_fault_stack_top(super::cpu_index());
    top >= 32768 && (top - 32768..top).contains(&sp)
}
/// Detach empty lower tables, retaining the root link shared by private
/// PML4s. Frames remain owned by the caller until the shootdown completes.
pub fn detach_empty(va: u64) -> [u64; 2] {
    use x86_64::structures::paging::PageTable;
    unsafe {
        let va = VirtAddr::new(va);
        let root = page_table::active_pml4();
        let pdpt = &mut *page_table::phys_to_virt(root[va.p4_index()].addr()).as_mut_ptr::<PageTable>();
        let mut retired = [0;2];
        let pd = &mut *page_table::phys_to_virt(pdpt[va.p3_index()].addr()).as_mut_ptr::<PageTable>();
        let pt = &mut *page_table::phys_to_virt(pd[va.p2_index()].addr()).as_mut_ptr::<PageTable>();
        if pt.iter().all(|entry| !entry.flags().contains(F::PRESENT)) {
            retired[0] = pd[va.p2_index()].addr().as_u64();
            pd[va.p2_index()].set_unused();
            if pd.iter().all(|entry| !entry.flags().contains(F::PRESENT)) {
                retired[1] = pdpt[va.p3_index()].addr().as_u64();
                pdpt[va.p3_index()].set_unused();
            }
        }
        retired
    }
}
pub fn emergency_byte(byte: u8) {
    use x86_64::instructions::port::Port;
    unsafe {
        for _ in 0..100000 {
            if Port::<u8>::new(0x3fd).read() & 0x20 != 0 {
                Port::<u8>::new(0x3f8).write(byte); break;
            }
            core::hint::spin_loop();
        }
    }
}
