//! arm64 memory glue: device-tree RAM → frame allocator, the identity
//! physical→virtual translation, and the kernel heap.

use crate::memory::frame_allocator::{FrameAllocator, PhysRange, FRAME_SIZE};

/// Initial heap size (16 MiB); it grows in place while the frames right
/// after it are free
const HEAP_INITIAL: u64 = 16 * 1024 * 1024;
const HEAP_GROW_CHUNK: u64 = 2 * 1024 * 1024;

/// Hand RAM to the portable memory manager. Everything from the start of
/// RAM to the end of the kernel image (the DTB QEMU places at RAM start,
/// the kernel, its boot stack) is reserved, as is the DTB wherever it is.
pub fn init(ram: &[(u64, u64)], dtb: u64, kernel_start: u64, kernel_end: u64) {
    let mut usable = [PhysRange { start: 0, end: 0 }; 4];
    let mut n = 0;
    for &(base, size) in ram {
        let end = (base + size).min(super::mmu::EARLY_RAM_END);
        if end > base && n < usable.len() {
            usable[n] = PhysRange { start: base, end };
            n += 1;
        }
    }
    let ram_start = ram.iter().map(|&(b, _)| b).min().unwrap_or(kernel_start);
    let dtb_end = dtb + super::fdt::total_size(dtb);
    let reserved = [
        PhysRange { start: ram_start.min(kernel_start), end: kernel_end },
        PhysRange { start: dtb, end: dtb_end },
    ];
    crate::memory::init(&usable[..n], &reserved, kernel_end);
}

/// Physical → kernel virtual: RAM and devices are identity-mapped
pub fn phys_to_virt(phys: u64) -> *mut u8 {
    phys as *mut u8
}

/// Carve the initial heap from contiguous physical frames
pub fn heap_init(fa: &mut FrameAllocator) -> (u64, u64) {
    let pa = fa
        .allocate_contiguous((HEAP_INITIAL / FRAME_SIZE) as usize)
        .expect("no contiguous RAM for the kernel heap");
    (pa, pa + HEAP_INITIAL)
}

/// Grow the heap in place by claiming the frames right after it. Called
/// under the frame-allocator lock; None if they're taken (heap exhausted).
pub fn heap_extend(end: u64, needed: u64, fa: &mut FrameAllocator) -> Option<u64> {
    let grow = (needed + HEAP_GROW_CHUNK - 1) / HEAP_GROW_CHUNK * HEAP_GROW_CHUNK;
    fa.claim_range(end, (grow / FRAME_SIZE) as usize).then_some(grow)
}

/// The kernel may touch user memory directly (PAN is not enabled)
pub fn user_access_begin() {}
pub fn user_access_end() {}
