//! Bounds and speculation masking, shared with host boundary regressions.
pub fn bounds_ok(ptr: u64, len: u64, base: u64, end: u64) -> bool {
    len == 0 || (ptr >= base && ptr < end && len <= end - ptr)
}
#[inline(always)]
pub fn mask_user_ptr(ptr: u64, len: u64, base: u64, end: u64) -> u64 {
    use crate::arch::nospec::array_index_mask_nospec;
    let start = array_index_mask_nospec(ptr.wrapping_sub(base) as usize, (end - base) as usize) as u64;
    let length = array_index_mask_nospec(len as usize, end.wrapping_sub(ptr).wrapping_add(1) as usize) as u64;
    let mask = start & length;
    (ptr & mask) | (end & !mask)
}
