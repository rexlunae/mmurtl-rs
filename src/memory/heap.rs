//! Kernel Heap Allocator — Simple bump allocator.
//!
//! Provides a global allocator for Rust's `alloc` crate, backed by physical
//! page allocations from the frame allocator. The bump allocator just
//! increments a pointer, making it fast but unable to free individual
//! allocations (dealloc is a no-op). This is fine until we need proper
//! memory recycling, at which point we'll upgrade to a slab/buddy allocator.

use core::alloc::{GlobalAlloc, Layout};
use core::sync::atomic::{AtomicBool, AtomicUsize, Ordering};

use crate::memory::frame_allocator::{self, FRAME_SIZE};
use x86_64::structures::paging::page_table::PageTableFlags as Flags;
use x86_64::VirtAddr;
use x86_64::structures::paging::Page;

/// Size of the initial kernel heap (4 MiB)
const INITIAL_HEAP_SIZE: u64 = 4 * 1024 * 1024;

/// Heap start virtual address (in the kernel's higher-half region)
const HEAP_START: u64 = 0xFFFF_9000_0000_0000;

/// Global frame allocator pointer — set once during memory init
static mut FRAME_ALLOC_PTR: *mut frame_allocator::FrameAllocator = core::ptr::null_mut();

/// Set the global frame allocator reference (called during memory init)
pub unsafe fn set_frame_allocator(fa: *mut frame_allocator::FrameAllocator) {
    FRAME_ALLOC_PTR = fa;
}

/// Run a closure with mutable access to the global frame allocator.
///
/// Returns None if memory management is not yet initialized. Callers must
/// run with interrupts disabled or during single-threaded init (the frame
/// allocator itself is not locked).
pub fn with_frame_allocator<R>(
    f: impl FnOnce(&mut frame_allocator::FrameAllocator) -> R,
) -> Option<R> {
    unsafe {
        let ptr = FRAME_ALLOC_PTR;
        if ptr.is_null() {
            None
        } else {
            Some(f(&mut *ptr))
        }
    }
}

// ========================================================================
// Bump Allocator
// ========================================================================

/// A simple bump-pointer allocator backed by page-level allocations.
///
/// `next_free` points to the next available byte in the current heap region.
/// When exhausted, we allocate more pages from the frame allocator.
pub struct BumpAllocator {
    initialized: AtomicBool,
    heap_start: AtomicUsize,
    heap_end: AtomicUsize,
    next_free: AtomicUsize,
}

unsafe impl Sync for BumpAllocator {}

impl BumpAllocator {
    pub const fn new() -> Self {
        Self {
            initialized: AtomicBool::new(false),
            heap_start: AtomicUsize::new(0),
            heap_end: AtomicUsize::new(0),
            next_free: AtomicUsize::new(0),
        }
    }

    /// Initialize the heap: allocate initial pages and set up bump pointer
    pub fn init(&self, frame_alloc: &mut frame_allocator::FrameAllocator) {
        if self.initialized.swap(true, Ordering::SeqCst) {
            return;
        }

        let start = HEAP_START;
        let end = HEAP_START + INITIAL_HEAP_SIZE;

        crate::serial::write_str("[HEAP] Bump allocator at 0x");
        crate::serial::write_hex(start);
        crate::serial::write_str(" (");
        crate::serial::write_dec(INITIAL_HEAP_SIZE / 1024);
        crate::serial::write_str(" KiB)\n");

        // Map the initial heap pages
        map_pages(start, INITIAL_HEAP_SIZE, frame_alloc);

        // Full TLB flush — ensure page table changes are seen by CPU
        unsafe {
            use x86_64::registers::control::Cr3;
            let (pml4_frame, flags) = Cr3::read();
            Cr3::write(pml4_frame, flags);
        }

        self.heap_start.store(start as usize, Ordering::SeqCst);
        self.heap_end.store(end as usize, Ordering::SeqCst);
        self.next_free.store(start as usize, Ordering::SeqCst);
    }

    /// Allocate memory — bump the pointer, extend if needed
    fn alloc_impl(&self, layout: Layout) -> *mut u8 {
        let size = layout.size();
        let align = layout.align();

        loop {
            let current = self.next_free.load(Ordering::Acquire);
            let aligned = (current + align - 1) & !(align - 1);
            let new_free = aligned + size;

            let heap_end = self.heap_end.load(Ordering::Acquire);

            if new_free <= heap_end {
                // Fast path: fits in current heap
                if self.next_free.compare_exchange(
                    current, new_free, Ordering::SeqCst, Ordering::Relaxed,
                ).is_ok() {
                    return aligned as *mut u8;
                }
                // CAS failed — retry
                continue;
            }

            // Need to extend — grab the frame allocator (single-core OK)
            unsafe {
                let fa = FRAME_ALLOC_PTR;
                if fa.is_null() {
                    return core::ptr::null_mut();
                }
                let fa = &mut *fa;

                // Calculate how many pages we need
                let needed = new_free - current;
                let total_extend = ((needed + (512 * FRAME_SIZE as usize) - 1) / (512 * FRAME_SIZE as usize))
                    * (512 * FRAME_SIZE as usize);

                let old_end = heap_end;
                let new_end = old_end + total_extend;

                crate::serial::write_str("[HEAP] Extending by ");
                crate::serial::write_dec((total_extend / 1024) as u64);
                crate::serial::write_str(" KiB\n");

                map_pages(old_end as u64, total_extend as u64, fa);

                self.heap_end.store(new_end, Ordering::SeqCst);
            }
            // Loop back and retry the allocation
        }
    }
}

/// Map a range of virtual pages to physical frames
fn map_pages(virt_start: u64, size: u64, frame_alloc: &mut frame_allocator::FrameAllocator) {
    use super::page_table;

    let num_pages = (size + FRAME_SIZE - 1) / FRAME_SIZE;
    let start_page = Page::containing_address(VirtAddr::new(virt_start));

    unsafe {
        let mut fa_adapter = frame_allocator::BumpFrameAllocator::new(frame_alloc);

        for i in 0..num_pages {
            let page = start_page + i;
            let frame = frame_alloc
                .allocate_frame()
                .expect("OOM during heap page allocation");

            page_table::map_page(
                page,
                frame,
                Flags::PRESENT | Flags::WRITABLE | Flags::NO_EXECUTE,
                &mut fa_adapter,
            )
            .expect("Failed to map heap page");
        }
    }
}

// ========================================================================
// GlobalAlloc trait implementation
// ========================================================================

unsafe impl GlobalAlloc for BumpAllocator {
    unsafe fn alloc(&self, layout: Layout) -> *mut u8 {
        self.alloc_impl(layout)
    }

    unsafe fn dealloc(&self, _ptr: *mut u8, _layout: Layout) {
        // Bump allocator: dealloc is a no-op
        // (memory will be usable again after a reset, or we'll add a proper
        //  allocator in a later phase)
    }
}

#[global_allocator]
static GLOBAL_ALLOC: BumpAllocator = BumpAllocator::new();

/// Initialize the kernel heap
pub fn init_heap(frame_alloc: &mut frame_allocator::FrameAllocator) {
    unsafe {
        set_frame_allocator(frame_alloc as *mut _);
    }
    GLOBAL_ALLOC.init(frame_alloc);
}


