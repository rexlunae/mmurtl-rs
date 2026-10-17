//! Pure layout shared by the allocator, entry bounds and host regressions.
pub const BASE: u64 = 0xffff_e000_0000_0000;
pub const LIMIT: u64 = BASE + (1 << 39);
pub const PAGE: u64 = 4096;
pub const SIZE: u64 = 32768;
pub const STRIDE: u64 = 65536;
const _: () = assert!(BASE >> 39 & 511 == 448 && SIZE + 2 * PAGE <= STRIDE);
pub fn bottom(frame: u64, first: u64, next: u64) -> Option<u64> {
    if frame < first + PAGE || frame >= next || next > LIMIT { return None; }
    let bottom = (frame & !(STRIDE - 1)) + PAGE;
    (frame >= bottom && frame < bottom + SIZE).then_some(bottom)
}
