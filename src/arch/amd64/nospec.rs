//! Data-dependent bounds masks, opaque to the compiler.
#[inline(always)]
pub fn array_index_mask_nospec(index: usize, size: usize) -> usize {
    let mask: usize;
    unsafe { core::arch::asm!("cmp {index}, {size}", "sbb {mask}, {mask}",
        index = in(reg) index, size = in(reg) size, mask = lateout(reg) mask,
        options(nostack)); }
    mask
}
#[inline(always)]
pub fn array_index_nospec(index: usize, size: usize) -> usize {
    index & array_index_mask_nospec(index, size)
}
#[inline(always)]
pub fn barrier_nospec() { unsafe { core::arch::asm!("lfence", options(nostack)); } }
