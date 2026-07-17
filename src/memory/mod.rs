//! Memory Management — Physical frame allocator, page table management, kernel heap.
//!
//! In long mode we use 4-level paging. This module initializes the page tables,
//! sets up a bitmap-based frame allocator, and provides a kernel heap for
//! dynamic allocation via the `alloc` crate.

use bootloader_api::BootInfo;
use bootloader_api::info::Optional;

pub mod frame_allocator;
pub mod page_table;
pub mod heap;

/// Initialize memory management from bootloader info
///
/// Must be called after serial init, before any dynamic allocations.
pub fn init(boot_info: &'static mut BootInfo) {
    crate::serial::write_str("[MEM] Initializing memory manager...\n");

    // 1. Initialize page table management (must know physical memory offset)
    let phys_offset = match boot_info.physical_memory_offset {
        Optional::Some(offset) => Some(offset),
        Optional::None => None,
    };
    page_table::init(phys_offset, boot_info);

    // 2. Initialize the bitmap-based frame allocator from memory regions
    let frame_allocator = frame_allocator::FrameAllocator::init(&boot_info.memory_regions);

    // 3. Initialize the kernel heap
    heap::init_heap(frame_allocator);

    // Log summary
    let free_mib = (frame_allocator.free_count() as u64 * frame_allocator::FRAME_SIZE) / (1024 * 1024);
    let total_mib = (frame_allocator.total_count() as u64 * frame_allocator::FRAME_SIZE) / (1024 * 1024);
    crate::serial::write_str("[MEM] Memory manager initialized: ");
    crate::serial::write_dec(free_mib);
    crate::serial::write_str(" MiB free / ");
    crate::serial::write_dec(total_mib);
    crate::serial::write_str(" MiB total\n");
}
