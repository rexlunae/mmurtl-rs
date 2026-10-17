//! Disjoint virtual-placement ranges; these functions add no entropy.
pub const AMD_DYNAMIC_START: u64 = 0xffff_8000_0000_0000;
pub const AMD_DYNAMIC_END: u64 = 0xffff_bfff_ffff_ffff;
pub const AMD_HEAP_BASE: u64 = 0xffff_c000_0000_0000;
pub const AMD_IST_BASE: u64 = 0xffff_c800_0000_0000;
pub const ARM_HEAP_BASE:u64=0xffff_d000_0000_0000;
// Covers the maximum 4096 slab spans, worst-case alignment padding and
// the final 2 MiB mapping chunk, even at the highest random placement.
pub const HEAP_VA_BUDGET:u64=(128<<30)+(2<<20);
pub const HEAP_PLACEMENTS:u64=(((4<<40)-HEAP_VA_BUDGET)>>21)+1;
pub fn amd_ist(word: u64) -> u64 { AMD_IST_BASE + ((word & ((1 << 21) - 1)) << 21) }
// Reserve space for the entire initial backing even at the final slot.
pub fn amd_heap(word: u64) -> u64 { AMD_HEAP_BASE + ((word % HEAP_PLACEMENTS) << 21) }
pub fn arm_heap(word:u64)->u64 {ARM_HEAP_BASE+((word%HEAP_PLACEMENTS)<<21)}
pub fn arm_layout(words: [u64; 3]) -> (u64, u64) {
    const HIGH: u64 = 0xffff_0000_0000_0000;
    let linear = HIGH | ((1 + (words[0] & 127)) << 39);
    let image = HIGH | ((256 + (words[1] & 127)) << 39) | ((words[2] & ((1 << 17) - 1)) << 21);
    (linear, image)
}
