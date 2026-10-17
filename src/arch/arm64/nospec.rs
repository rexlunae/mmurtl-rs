//! CSDB constrains consumption of the masked index. Full barriers are
//! used at control-dependency boundaries such as lock acquisition.
#[inline(always)]
pub fn array_index_mask_nospec(index: usize, size: usize) -> usize {
    let mask: usize;
    unsafe { core::arch::asm!("cmp {index}, {size}", "csetm {mask}, lo", "csdb",
        index = in(reg) index, size = in(reg) size, mask = lateout(reg) mask,
        options(nostack)); }
    mask
}
#[inline(always)]
pub fn array_index_nospec(index: usize, size: usize) -> usize {
    index & array_index_mask_nospec(index, size)
}
#[inline(always)]
pub fn barrier_nospec() { unsafe { core::arch::asm!("dsb sy", "isb", options(nostack)); } }
