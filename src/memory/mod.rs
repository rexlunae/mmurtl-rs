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

/// Ensure a physical range is accessible through the physical-memory offset
/// mapping, mapping any missing pages on demand.
///
/// With `uncached` (for MMIO: Local APIC, I/O APIC) the pages are also
/// guaranteed to be uncacheable. That matters even when the range is
/// *already* mapped: the bootloader's `Mapping::Dynamic` window covers
/// 0..max_phys_addr as cacheable write-back, so on machines with enough
/// RAM the MMIO window is pre-mapped with the wrong attributes (and often
/// inside a huge page). Such pages get their attributes forced to UC,
/// splitting the covering huge page so only the 4 KiB MMIO page changes.
pub fn ensure_phys_mapped(phys_start: u64, len: u64, uncached: bool) {
    use x86_64::structures::paging::{Page, PhysFrame};
    use x86_64::structures::paging::page_table::PageTableFlags as Flags;
    use x86_64::{PhysAddr, VirtAddr};

    let start = phys_start & !0xFFF;
    let end = (phys_start + len + 0xFFF) & !0xFFF;

    let mut phys = start;
    while phys < end {
        let virt = page_table::phys_to_virt(PhysAddr::new(phys));
        let already_mapped = page_table::translate_virtual(virt).is_some();

        if !already_mapped {
            let mut flags = Flags::PRESENT | Flags::WRITABLE | Flags::NO_EXECUTE;
            if uncached {
                flags |= Flags::NO_CACHE | Flags::WRITE_THROUGH;
            }
            heap::with_frame_allocator(|fa| {
                let mut adapter = frame_allocator::BumpFrameAllocator::new(fa);
                unsafe {
                    page_table::map_page(
                        Page::containing_address(VirtAddr::new(virt.as_u64())),
                        PhysFrame::containing_address(PhysAddr::new(phys)),
                        flags,
                        &mut adapter,
                    )
                    .expect("Failed to map physical range");
                }
            })
            .expect("Frame allocator not initialized");
        } else if uncached && !page_is_uncached(virt) {
            heap::with_frame_allocator(|fa| {
                let mut adapter = frame_allocator::BumpFrameAllocator::new(fa);
                unsafe {
                    page_table::set_page_uncached(virt, &mut adapter)
                        .expect("Failed to force UC on mapped MMIO page");
                }
            })
            .expect("Frame allocator not initialized");

            crate::serial::write_str("[PAGING] Forced UC on pre-mapped MMIO page phys 0x");
            crate::serial::write_hex(phys);
            crate::serial::write_str("\n");
        }
        phys += 4096;
    }
}

/// Whether the mapping covering `virt` already has cache-disable set
fn page_is_uncached(virt: x86_64::VirtAddr) -> bool {
    use x86_64::structures::paging::page_table::PageTableFlags as Flags;
    match page_table::query_page(virt) {
        Some(flags) => flags.contains(Flags::NO_CACHE),
        None => false,
    }
}

/// Identity-map a single executable page (virt == phys).
///
/// Used for the SMP AP trampoline: after an AP enables paging with the
/// kernel's CR3, it is still executing at its low physical address, so that
/// address must be identity-mapped and executable.
pub fn identity_map_executable(phys: u64) {
    use x86_64::structures::paging::{Page, PhysFrame};
    use x86_64::structures::paging::page_table::PageTableFlags as Flags;
    use x86_64::{PhysAddr, VirtAddr};

    if page_table::translate_virtual(VirtAddr::new(phys)).is_some() {
        return;
    }
    heap::with_frame_allocator(|fa| {
        let mut adapter = frame_allocator::BumpFrameAllocator::new(fa);
        unsafe {
            page_table::map_page(
                Page::containing_address(VirtAddr::new(phys)),
                PhysFrame::containing_address(PhysAddr::new(phys)),
                Flags::PRESENT | Flags::WRITABLE,
                &mut adapter,
            )
            .expect("Failed to identity-map trampoline page");
        }
    })
    .expect("Frame allocator not initialized");
}
