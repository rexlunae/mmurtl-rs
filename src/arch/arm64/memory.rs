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
    (phys + physical_offset()) as *mut u8
}

/// Carve the initial heap from contiguous physical frames
pub fn heap_init(fa: &mut FrameAllocator) -> (u64, u64) {
    let pa = fa
        .allocate_zeroed_contiguous((HEAP_INITIAL / FRAME_SIZE) as usize)
        .expect("no contiguous RAM for the kernel heap");
    (pa + physical_offset(), pa + physical_offset() + HEAP_INITIAL)
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
const HIGH_START: u64 = 0xffff_0000_0000_0000;
use core::sync::atomic::{AtomicU64, Ordering};
static PHYSICAL_OFFSET: AtomicU64 = AtomicU64::new(HIGH_START);
static KERNEL_OFFSET: AtomicU64 = AtomicU64::new(HIGH_START);
pub fn physical_offset() -> u64 { PHYSICAL_OFFSET.load(Ordering::Relaxed) }
pub fn kernel_offset() -> u64 { KERNEL_OFFSET.load(Ordering::Relaxed) }
/// Once before translation or AP release. Separate L0 regions never overlap.
pub fn choose_layout(load_addr: u64) {
    let (linear, image) = crate::security::kaslr::arm_layout([
        crate::random::early_u64(), crate::random::early_u64(), crate::random::early_u64()
    ]);
    PHYSICAL_OFFSET.store(linear, Ordering::Relaxed);
    KERNEL_OFFSET.store(image - (load_addr & !((2 << 20) - 1)), Ordering::Relaxed);
}
pub fn virt_to_phys(va: u64) -> u64 {
    if va < HIGH_START { return va; }
    let linear = physical_offset();
    if (linear..linear + (1 << 39)).contains(&va) { va - linear }
    else { va.checked_sub(kernel_offset()).expect("unknown kernel virtual address") }
}
/// Setup walks physical tables; runtime walks use their linear alias.
pub fn boot_pointer(pa: u64) -> *mut u8 {
    let pc: u64;
    unsafe { core::arch::asm!("adr {}, .", out(reg) pc, options(nomem, nostack)); }
    if pc >= HIGH_START { phys_to_virt(pa) } else { pa as *mut u8 }
}
