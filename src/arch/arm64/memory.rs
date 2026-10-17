//! arm64 memory glue: device-tree RAM, the high TTBR1 physical window and heap.

use crate::memory::frame_allocator::{FrameAllocator, PhysRange, FRAME_SIZE};

/// Initial heap size (16 MiB); it grows in place while the frames right
/// after it are free
const HEAP_INITIAL: u64 = 16 * 1024 * 1024;
const HEAP_GROW_CHUNK: u64 = 2 * 1024 * 1024;

/// Hand RAM to the portable memory manager. Reserved: everything from the
/// start of RAM's first range up to the kernel if the kernel sits within
/// its first 2 MiB (where QEMU puts the DTB and the Pi firmware its spin
/// tables), otherwise just the kernel image and boot stack; the DTB
/// wherever it is; and the device tree's /memreserve/ entries.
pub fn init(ram: &[(u64, u64)], memreserve: &[(u64, u64)], dtb: u64, kernel_start: u64, kernel_end: u64) {
    let mut usable = [PhysRange { start: 0, end: 0 }; 8];
    let mut n = 0;
    for &(base, size) in ram {
        if size > 0 && n < usable.len() {
            usable[n] = PhysRange { start: base, end: base + size };
            n += 1;
        }
    }
    let ram_start = ram.iter().map(|&(b, _)| b).min().unwrap_or(kernel_start);
    let low = if kernel_start - ram_start.min(kernel_start) <= 0x20_0000 {
        ram_start.min(kernel_start)
    } else {
        kernel_start
    };
    let mut reserved = [PhysRange { start: 0, end: 0 }; 10];
    reserved[0] = PhysRange { start: low, end: kernel_end };
    reserved[1] = PhysRange { start: dtb, end: dtb + super::fdt::total_size(dtb) };
    let mut r = 2;
    for &(base, size) in memreserve {
        if size > 0 && r < reserved.len() {
            reserved[r] = PhysRange { start: base, end: base + size };
            r += 1;
        }
    }
    crate::memory::init(&usable[..n], &reserved[..r], virt_to_phys(kernel_end));
}

/// Kernel RAM/device window in TTBR1.
pub fn phys_to_virt(phys: u64) -> *mut u8 {
    (phys + PHYS_OFFSET) as *mut u8
}

/// Carve the initial heap from contiguous physical frames
pub fn heap_init(fa: &mut FrameAllocator) -> (u64, u64) {
    let pa = fa
        .allocate_zeroed_contiguous((HEAP_INITIAL / FRAME_SIZE) as usize)
        .expect("no contiguous RAM for the kernel heap");
    (pa + PHYS_OFFSET, pa + PHYS_OFFSET + HEAP_INITIAL)
}

/// Grow the heap in place by claiming the frames right after it. Called
/// under the frame-allocator lock; None if they're taken (heap exhausted).
pub fn heap_extend(end: u64, needed: u64, fa: &mut FrameAllocator) -> Option<u64> {
    let grow = (needed + HEAP_GROW_CHUNK - 1) / HEAP_GROW_CHUNK * HEAP_GROW_CHUNK;
    fa.claim_range(virt_to_phys(end), (grow / FRAME_SIZE) as usize).then_some(grow)
}

/// IRQ-masked, non-nested user access windows (normally use uaccess wrappers).
pub fn user_access_begin() { super::pan::begin(); }
pub fn user_access_end() { super::pan::end(); }

/// 48-bit upper translation regime, independent of the physical image address.
pub const PHYS_OFFSET: u64 = 0xffff_0000_0000_0000;
pub fn virt_to_phys(va: u64) -> u64 {
    if va >= PHYS_OFFSET { va - PHYS_OFFSET } else { va }
}
/// Page-table setup runs before the high jump; runtime walks run after it.
pub fn boot_pointer(pa: u64) -> *mut u8 {
    let pc: u64;
    unsafe { core::arch::asm!("adr {}, .", out(reg) pc, options(nomem, nostack)); }
    if pc >= PHYS_OFFSET { phys_to_virt(pa) } else { pa as *mut u8 }
}
