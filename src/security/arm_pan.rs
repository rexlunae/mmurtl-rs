//! ARM access-protection capability and saved-PSTATE rules, without I/O.
use super::types::feature as F;
pub fn features(mmfr1: u64, mmfr2: u64) -> u64 {
    let pan = (mmfr1 >> 20) & 15;
    let mut flags = 0;
    if matches!(pan, 1..=3) { flags |= F::PAN; }
    if pan == 3 { flags |= F::EPAN; }
    if (mmfr2 >> 4) & 15 == 1 { flags |= F::UAO; }
    flags
}
pub fn return_spsr(old: u64, hardware_pan: bool) -> u64 {
    let mut new = old & !((1 << 22) | (1 << 23)); // no inherited UAO or user PAN
    if hardware_pan && old & 15 != 0 { new |= 1 << 22; }
    new
}
