//! CR4 access-protection policy, independent of privileged instructions.
pub const SMEP: u64 = 1 << 20;
pub const SMAP: u64 = 1 << 21;
pub const UMIP: u64 = 1 << 11;
pub const LASS: u64 = 1 << 27;
pub const ALL: u64 = SMEP | SMAP | UMIP | LASS;
pub fn common(mut cpus: impl Iterator<Item = u64>) -> u64 {
    let Some(first) = cpus.next() else { return 0; };
    let mut result = cpus.fold(first & ALL, |common, cpu| common & cpu);
    // Supervisor LASS data checks require SMAP; retain one uniform
    // STAC/CLAC policy on all CPUs rather than partially enabling LASS.
    if result & SMAP == 0 { result &= !LASS; }
    result
}
