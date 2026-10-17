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
