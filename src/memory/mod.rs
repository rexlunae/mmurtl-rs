//! Memory Management — physical frame allocator, kernel heap, user memory.
//!
//! Architecture-neutral. The architecture supplies the usable RAM ranges,
//! the physical→virtual translation, the heap's backing region, and the
//! page-table operations used for user memory (`crate::arch`).

pub mod frame_allocator;
pub mod heap;
pub mod user;

pub use frame_allocator::PhysRange;

/// Initialize the frame allocator and kernel heap.
///
/// `usable` lists RAM the kernel may hand out; `reserved` carves holes in
/// it (low memory, boot structures); the allocator's own bitmap is placed
/// at or above `min_addr`. Must be called after serial init, before any
/// dynamic allocation.
pub fn init(usable: &[PhysRange], reserved: &[PhysRange], min_addr: u64) {
    crate::serial::write_str("[MEM] Initializing memory manager...\n");

    let frame_allocator = frame_allocator::FrameAllocator::init(usable, reserved, min_addr);
    heap::init_heap(frame_allocator);

    let free_mib = (frame_allocator.free_count() as u64 * frame_allocator::FRAME_SIZE) / (1024 * 1024);
    let usable_mib = usable.iter().map(|r| r.end - r.start).sum::<u64>() / (1024 * 1024);
    crate::serial::write_str("[MEM] Memory manager initialized: ");
    crate::serial::write_dec(free_mib);
    crate::serial::write_str(" MiB free / ");
    crate::serial::write_dec(usable_mib);
    crate::serial::write_str(" MiB usable RAM\n");
}
