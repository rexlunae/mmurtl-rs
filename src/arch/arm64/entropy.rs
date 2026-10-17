//! RNDR is optional; unsupported systems keep uncredited timing input.
pub fn counter() -> u64 {
    let value: u64; unsafe { core::arch::asm!("mrs {}, cntvct_el0", out(reg) value, options(nomem, nostack)); } value
}
pub fn collect() {
    let isar: u64;
    unsafe { core::arch::asm!("mrs {}, id_aa64isar0_el1", out(reg) isar, options(nomem, nostack)); }
    if (isar >> 60) & 15 == 1 {
        for reseed in [true, false] {
            let mut bytes = [0u8; 32];
            let mut complete = true;
            for chunk in bytes.chunks_mut(8) {
                let mut got = false;
                for _ in 0..64 {
                    let value: u64; let valid: u64;
                    unsafe {
                        if reseed { core::arch::asm!("mrs {}, S3_3_C2_C4_1", "cset {}, ne", out(reg) value, out(reg) valid, options(nomem, nostack)); }
                        else { core::arch::asm!("mrs {}, S3_3_C2_C4_0", "cset {}, ne", out(reg) value, out(reg) valid, options(nomem, nostack)); }
                    }
                    if valid != 0 { chunk.copy_from_slice(&value.to_le_bytes()); got = true; break; }
                    core::hint::spin_loop();
                }
                complete &= got;
            }
            crate::random::seed(if reseed { "RNDRRS" } else { "RNDR" }, &mut bytes, if complete { 256 } else { 0 });
        }
    }
    if let Some(mut bytes) = super::cpu_features::trng_seed() {
        crate::random::seed("SMCCC TRNG", &mut bytes, 256);
    }
}
