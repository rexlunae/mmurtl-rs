//! Kernel Heap Allocator — Simple bump allocator.
//!
//! Provides the global allocator for Rust's `alloc` crate. The bump
//! allocator just increments a pointer, making it fast but unable to free
//! individual allocations (dealloc is a no-op).
//!
//! Where the heap lives is the architecture's business
//! (`crate::arch::heap_init` / `heap_extend`): amd64 maps fresh frames at a
//! fixed higher-half address and can grow the heap page by page; arm64
//! carves one physically contiguous, identity-mapped block.

use core::alloc::{GlobalAlloc, Layout};
use core::sync::atomic::{AtomicBool, AtomicUsize, Ordering};

use crate::memory::frame_allocator;

/// Global frame allocator pointer — set once during memory init
static mut FRAME_ALLOC_PTR: *mut frame_allocator::FrameAllocator = core::ptr::null_mut();

/// Serializes every use of the frame allocator across CPUs. Always taken
/// with interrupts disabled. Closures run under it must not allocate from
/// the kernel heap (heap growth takes this lock too).
static FRAME_LOCK: spin::Mutex<()> = spin::Mutex::new(());

/// Set the global frame allocator reference (called during memory init)
pub unsafe fn set_frame_allocator(fa: *mut frame_allocator::FrameAllocator) {
    FRAME_ALLOC_PTR = fa;
}

/// Run a closure with mutable access to the global frame allocator.
///
/// Returns None if memory management is not yet initialized. Safe to call
/// from any CPU: access is serialized by FRAME_LOCK (interrupts disabled
/// while held). The closure must not allocate from the kernel heap.
pub fn with_frame_allocator<R>(
    f: impl FnOnce(&mut frame_allocator::FrameAllocator) -> R,
) -> Option<R> {
    crate::arch::without_interrupts(|| {
        let _guard = FRAME_LOCK.lock();
        unsafe {
            let ptr = FRAME_ALLOC_PTR;
            if ptr.is_null() {
                None
            } else {
                Some(f(&mut *ptr))
            }
        }
    })
}

// ========================================================================
// Bump Allocator
// ========================================================================

/// A simple bump-pointer allocator over an architecture-provided region.
pub struct BumpAllocator {
    initialized: AtomicBool,
    heap_end: AtomicUsize,
    next_free: AtomicUsize,
}

unsafe impl Sync for BumpAllocator {}

impl BumpAllocator {
    pub const fn new() -> Self {
        Self {
            initialized: AtomicBool::new(false),
            heap_end: AtomicUsize::new(0),
            next_free: AtomicUsize::new(0),
        }
    }

    /// Initialize the heap over the architecture's initial region
    pub fn init(&self, frame_alloc: &mut frame_allocator::FrameAllocator) {
        if self.initialized.swap(true, Ordering::SeqCst) {
            return;
        }
        let (start, end) = crate::arch::heap_init(frame_alloc);

        crate::serial::write_str("[HEAP] Bump allocator at 0x");
        crate::serial::write_hex(start);
        crate::serial::write_str(" (");
        crate::serial::write_dec((end - start) / 1024);
        crate::serial::write_str(" KiB)\n");

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

            // Need to extend. Two CPUs can get here at once, so extension
            // happens under FRAME_LOCK, and only if nobody else extended
            // the heap since we sampled heap_end (otherwise just retry).
            let extended = crate::arch::without_interrupts(|| {
                let _guard = FRAME_LOCK.lock();
                if self.heap_end.load(Ordering::Acquire) != heap_end {
                    return Some(0); // raced: someone else grew the heap
                }
                unsafe {
                    let fa = FRAME_ALLOC_PTR;
                    if fa.is_null() {
                        return None;
                    }
                    let needed = (new_free - current) as u64;
                    let grown = crate::arch::heap_extend(heap_end as u64, needed, &mut *fa)?;
                    self.heap_end.store(heap_end + grown as usize, Ordering::SeqCst);
                    Some(grown)
                }
            });
            match extended {
                None => return core::ptr::null_mut(),
                Some(0) => {}
                Some(bytes) => {
                    crate::serial::write_str("[HEAP] Extended by ");
                    crate::serial::write_dec(bytes / 1024);
                    crate::serial::write_str(" KiB\n");
                }
            }
            // Loop back and retry the allocation
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
