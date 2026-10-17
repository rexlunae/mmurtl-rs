//! Bounded, CPUID-gated hardware entropy collection.
pub fn counter() -> u64 { unsafe { core::arch::x86_64::_rdtsc() } }
pub fn collect() {
    use core::arch::{asm, x86_64::__cpuid};
    let max = unsafe { __cpuid(0).eax };
    let rdrand = max >= 1 && unsafe { __cpuid(1).ecx & (1 << 30) != 0 };
    let rdseed = max >= 7 && unsafe { __cpuid(7).ebx & (1 << 18) != 0 };
    // RDRAND receives only 128 bits of conditional credit; public jitter
    // is mixed separately and receives none. Broken/stuck samples get none.
    for (seed, present, name, credit) in [(true, rdseed, "RDSEED", 256), (false, rdrand, "RDRAND", 128)] {
        if !present { continue; }
        let mut bytes = [0u8; 32];
        let mut complete = true;
        for chunk in bytes.chunks_mut(8) {
            let mut got = false;
            for _ in 0..64 {
                let value: u64; let valid: u8;
                unsafe {
                    if seed { asm!("rdseed {}", "setc {}", out(reg) value, out(reg_byte) valid, options(nomem, nostack)); }
                    else { asm!("rdrand {}", "setc {}", out(reg) value, out(reg_byte) valid, options(nomem, nostack)); }
                }
                if valid != 0 { chunk.copy_from_slice(&value.to_le_bytes()); got = true; break; }
                core::hint::spin_loop();
            }
            complete &= got;
        }
        crate::random::seed(name, &mut bytes, if complete { credit } else { 0 });
    }
}
