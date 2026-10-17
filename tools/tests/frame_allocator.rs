//! Host regression for the real allocator against a simulated dirty physmap.
use std::sync::atomic::{AtomicPtr, Ordering};
static RAM: AtomicPtr<u8> = AtomicPtr::new(std::ptr::null_mut());
mod arch {
    pub fn phys_to_virt(pa: u64) -> *mut u8 {
        unsafe { super::RAM.load(super::Ordering::Relaxed).add(pa as usize) }
    }
}
mod serial {
    pub fn write_str(_: &str) {}
    pub fn write_line(_: &str) {}
    pub fn write_hex(_: u64) {}
    pub fn write_dec(_: u64) {}
}
#[path = "../../src/memory/frame_allocator.rs"]
mod frame_allocator;
use frame_allocator::{FrameAllocator, PhysRange};
fn main() {
    let layout = std::alloc::Layout::from_size_align(16 * 1024 * 1024, 4096).unwrap();
    let ram = unsafe { std::alloc::alloc(layout) };
    assert!(!ram.is_null());
    RAM.store(ram, Ordering::Relaxed);
    unsafe { ram.write_bytes(0xa5, layout.size()); }
    let fa = FrameAllocator::init(&[PhysRange { start: 0, end: layout.size() as u64 }],
        &[PhysRange { start: 0, end: 4096 }, PhysRange { start: 8192, end: 3 * 1024 * 1024 }], 4096);
    // Metadata placement must not overwrite reserved firmware memory.
    assert!(unsafe { std::slice::from_raw_parts(ram.add(8192), 4096) }.iter().all(|b| *b == 0xa5));
    let before = fa.free_count();
    let pa = fa.allocate_zeroed_frame().unwrap();
    let ptr = arch::phys_to_virt(pa);
    assert!(unsafe { std::slice::from_raw_parts(ptr, 4096) }.iter().all(|b| *b == 0));
    unsafe { ptr.write_bytes(0x5e, 4096); }
    fa.deallocate_frame(pa);
    assert!(unsafe { std::slice::from_raw_parts(ptr, 4096) }.iter().all(|b| *b == 0));
    assert_eq!(fa.free_count(), before);
    assert!(fa.released_frames_are_zero());
    unsafe { ptr.write_volatile(1); }
    assert!(!fa.released_frames_are_zero());
    unsafe { ptr.write_volatile(0); }
    // Claiming a specific range (heap growth) also returns zeroed memory.
    assert!(fa.claim_range(pa, 1));
    assert!(unsafe { std::slice::from_raw_parts(ptr, 4096) }.iter().all(|b| *b == 0));
    assert!(!fa.claim_range(pa, 1));
    assert!(!fa.claim_range(pa + 1, 1));
    assert!(!fa.claim_range(u64::MAX & !4095, usize::MAX));
    fa.deallocate_frame(pa);
    let contiguous = fa.allocate_zeroed_contiguous(8).unwrap();
    let ptr = arch::phys_to_virt(contiguous);
    assert!(unsafe { std::slice::from_raw_parts(ptr, 8 * 4096) }.iter().all(|b| *b == 0));
    unsafe { ptr.write_bytes(0x5e, 8 * 4096); }
    for n in 0..8 { fa.deallocate_frame(contiguous + n * 4096); }
    assert!(fa.released_frames_are_zero());
    assert_eq!(fa.free_count(), before);
    assert!(fa.allocate_zeroed_contiguous(0).is_none());
    assert!(fa.allocate_zeroed_contiguous(usize::MAX).is_none());
    fa.scrub_free_memory();
    assert!(fa.released_frames_are_zero());
    println!("frame allocator: dirty allocation, release scrub, heap claims, contiguous reuse, reservations passed");
}
